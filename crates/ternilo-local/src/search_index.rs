use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ternilo_protocol::{HarnessError, SessionEvent, SessionEventCategory, SessionSearchRequest};
use tokio_rusqlite::{
    Connection, params,
    rusqlite::{self, params_from_iter, types::Value as SqlValue},
};

use crate::{LocalSession, state::LocalState};

mod file_inventory;

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS search_events (
    rowid INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq >= 0),
    run_id TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms >= 0),
    category TEXT NOT NULL,
    body TEXT NOT NULL,
    UNIQUE (session_id, seq)
);

CREATE INDEX IF NOT EXISTS search_events_session_seq
ON search_events (session_id, seq);

CREATE INDEX IF NOT EXISTS search_events_filters
ON search_events (workspace_id, run_id, category, occurred_at_ms);

CREATE VIRTUAL TABLE IF NOT EXISTS search_events_fts USING fts5(
    body,
    content='search_events',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS search_events_insert AFTER INSERT ON search_events BEGIN
    INSERT INTO search_events_fts(rowid, body) VALUES (new.rowid, new.body);
END;

CREATE TRIGGER IF NOT EXISTS search_events_delete AFTER DELETE ON search_events BEGIN
    INSERT INTO search_events_fts(search_events_fts, rowid, body)
    VALUES ('delete', old.rowid, old.body);
END;

CREATE TRIGGER IF NOT EXISTS search_events_update AFTER UPDATE ON search_events BEGIN
    INSERT INTO search_events_fts(search_events_fts, rowid, body)
    VALUES ('delete', old.rowid, old.body);
    INSERT INTO search_events_fts(rowid, body) VALUES (new.rowid, new.body);
END;
";

#[derive(Clone, Debug)]
pub(crate) struct IndexedSearchHit {
    pub session_id: String,
    pub workspace_id: String,
    pub seq: u64,
    pub run_id: String,
    pub occurred_at_ms: u64,
    pub category: SessionEventCategory,
    pub excerpt: String,
}

pub(crate) struct LocalSearchIndex {
    connection: Connection,
    inbox: std::sync::Arc<crate::inbox::LocalInboxStore>,
    dirty: AtomicBool,
}

impl LocalSearchIndex {
    pub(crate) async fn open(
        path: &Path,
        inbox: std::sync::Arc<crate::inbox::LocalInboxStore>,
    ) -> Result<Self, HarnessError> {
        let connection = Connection::open(path).await.map_err(|error| {
            HarnessError::execution(format!(
                "open local session search index {}: {error}",
                path.display()
            ))
        })?;
        set_private_permissions(path).await?;
        connection
            .call(|database| -> Result<(), HarnessError> {
                database
                    .busy_timeout(Duration::from_secs(5))
                    .map_err(sqlite_error)?;
                database
                    .pragma_update(None, "journal_mode", "WAL")
                    .map_err(sqlite_error)?;
                database
                    .pragma_update(None, "synchronous", "FULL")
                    .map_err(sqlite_error)?;
                database.execute_batch(SCHEMA).map_err(sqlite_error)?;
                database
                    .execute_batch(file_inventory::SCHEMA)
                    .map_err(sqlite_error)
            })
            .await
            .map_err(worker_error)?;
        Ok(Self {
            connection,
            inbox,
            dirty: AtomicBool::new(true),
        })
    }

    pub(crate) async fn rebuild(&self, state: &LocalState) -> Result<(), HarnessError> {
        let sessions = state.snapshot().await.sessions;
        let mut documents = Vec::new();
        let mut files = Vec::new();
        for session in sessions {
            let events = crate::event_store::JsonlEventStore::new(
                &state.sessions_dir(),
                &session.identity.session_id,
            )
            .load_events()
            .await?;
            documents.extend(events.iter().map(|event| indexed_document(&session, event)));
            files.extend(file_inventory::documents(
                session.identity.session_id.as_str(),
                session.workspace_id.as_str(),
                &events,
            ));
            files.extend(file_inventory::upload_documents(
                session.identity.session_id.as_str(),
                session.workspace_id.as_str(),
                &self
                    .inbox
                    .accepted_uploads(session.identity.session_id.as_str())
                    .await?,
            ));
        }
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            transaction
                .execute("DELETE FROM search_events", [])
                .map_err(sqlite_error)?;
            transaction
                .execute("DELETE FROM session_files", [])
                .map_err(sqlite_error)?;
            file_inventory::insert(&transaction, &files)?;
            {
                let mut statement = transaction
                    .prepare(
                        "INSERT INTO search_events
                         (session_id, workspace_id, seq, run_id, occurred_at_ms, category, body)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    )
                    .map_err(sqlite_error)?;
                for document in documents {
                    statement
                        .execute(params![
                            document.session_id,
                            document.workspace_id,
                            to_i64(document.seq, "session event sequence")?,
                            document.run_id,
                            to_i64(document.occurred_at_ms, "session event timestamp")?,
                            document.category,
                            document.body,
                        ])
                        .map_err(sqlite_error)?;
                }
            }
            transaction.commit().map_err(sqlite_error)
        })
        .await?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn refresh_if_dirty(&self, state: &LocalState) -> Result<(), HarnessError> {
        if self.dirty.load(Ordering::Acquire) {
            self.rebuild(state).await?;
        }
        Ok(())
    }

    pub(crate) async fn append_fail_soft(
        &self,
        session_id: String,
        workspace_id: String,
        event: SessionEvent,
    ) {
        self.append_many_fail_soft(session_id, workspace_id, std::slice::from_ref(&event))
            .await;
    }

    /// Index an existing history prefix in one SQLite transaction. Fork used
    /// to await one FULL-synchronous autocommit per event, making its latency
    /// grow sharply with conversation length even though the index is a
    /// rebuildable cache.
    pub(crate) async fn append_many_fail_soft(
        &self,
        session_id: String,
        workspace_id: String,
        events: &[SessionEvent],
    ) {
        if events.is_empty() {
            return;
        }
        let documents = events
            .iter()
            .map(|event| IndexedDocument {
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
                seq: event.seq,
                run_id: event.run_id.as_str().to_owned(),
                occurred_at_ms: event.occurred_at_ms,
                category: category_name(event_category(&event.kind)).to_owned(),
                body: searchable_event_text(event),
            })
            .collect::<Vec<_>>();
        let files = file_inventory::documents(&session_id, &workspace_id, events);
        let result = self
            .call(move |database| {
                let transaction = database
                    .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                    .map_err(sqlite_error)?;
                file_inventory::insert(&transaction, &files)?;
                {
                    let mut statement = transaction
                        .prepare(
                            "INSERT INTO search_events
                         (session_id, workspace_id, seq, run_id, occurred_at_ms, category, body)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT(session_id, seq) DO UPDATE SET
                           workspace_id=excluded.workspace_id,
                           run_id=excluded.run_id,
                           occurred_at_ms=excluded.occurred_at_ms,
                           category=excluded.category,
                           body=excluded.body",
                        )
                        .map_err(sqlite_error)?;
                    for document in documents {
                        statement
                            .execute(params![
                                document.session_id,
                                document.workspace_id,
                                to_i64(document.seq, "session event sequence")?,
                                document.run_id,
                                to_i64(document.occurred_at_ms, "session event timestamp")?,
                                document.category,
                                document.body,
                            ])
                            .map_err(sqlite_error)?;
                    }
                }
                transaction.commit().map_err(sqlite_error)
            })
            .await;
        if result.is_err() {
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub(crate) async fn delete_fail_soft(&self, session_id: String) {
        let result = self
            .call(move |database| {
                let transaction = database.transaction().map_err(sqlite_error)?;
                transaction
                    .execute(
                        "DELETE FROM search_events WHERE session_id = ?1",
                        [&session_id],
                    )
                    .map_err(sqlite_error)?;
                transaction
                    .execute(
                        "DELETE FROM session_files WHERE session_id=?1",
                        [&session_id],
                    )
                    .map_err(sqlite_error)?;
                transaction.commit().map_err(sqlite_error)
            })
            .await;
        if result.is_err() {
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub(crate) async fn search(
        &self,
        request: &SessionSearchRequest,
        limit: usize,
    ) -> Result<Vec<IndexedSearchHit>, HarnessError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = literal_fts_query(&request.query);
        let request = request.clone();
        self.call(move |database| {
            let mut sql = String::from(
                "SELECT e.session_id, e.workspace_id, e.seq, e.run_id, e.occurred_at_ms,
                        e.category,
                        snippet(search_events_fts, 0, '', '', ' … ', 32)
                 FROM search_events_fts
                 JOIN search_events e ON e.rowid = search_events_fts.rowid
                 WHERE search_events_fts MATCH ?",
            );
            let mut parameters = vec![SqlValue::Text(query)];
            if let Some(session_id) = request.session_id {
                sql.push_str(" AND e.session_id = ?");
                parameters.push(SqlValue::Text(session_id.as_str().to_owned()));
            }
            if let Some(workspace_id) = request.workspace_id {
                sql.push_str(" AND e.workspace_id = ?");
                parameters.push(SqlValue::Text(workspace_id.as_str().to_owned()));
            }
            if let Some(run_id) = request.filters.run_id {
                sql.push_str(" AND e.run_id = ?");
                parameters.push(SqlValue::Text(run_id.as_str().to_owned()));
            }
            if let Some(category) = request.filters.category {
                sql.push_str(" AND e.category = ?");
                parameters.push(SqlValue::Text(category_name(category).to_owned()));
            }
            if let Some(after) = request.filters.occurred_after_ms {
                sql.push_str(" AND e.occurred_at_ms >= ?");
                parameters.push(SqlValue::Integer(to_i64(after, "search lower timestamp")?));
            }
            if let Some(before) = request.filters.occurred_before_ms {
                sql.push_str(" AND e.occurred_at_ms <= ?");
                parameters.push(SqlValue::Integer(to_i64(before, "search upper timestamp")?));
            }
            sql.push_str(" ORDER BY bm25(search_events_fts), e.occurred_at_ms DESC LIMIT ?");
            parameters.push(SqlValue::Integer(i64::try_from(limit).map_err(|_| {
                HarnessError::invalid("session search limit exceeds SQLite integer range")
            })?));
            let mut statement = database.prepare(&sql).map_err(sqlite_error)?;
            let rows = statement
                .query_map(params_from_iter(parameters), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })
                .map_err(sqlite_error)?;
            rows.map(|row| {
                let (session_id, workspace_id, seq, run_id, occurred_at_ms, category, excerpt) =
                    row.map_err(sqlite_error)?;
                Ok(IndexedSearchHit {
                    session_id,
                    workspace_id,
                    seq: from_i64(seq, "session event sequence")?,
                    run_id,
                    occurred_at_ms: from_i64(occurred_at_ms, "session event timestamp")?,
                    category: parse_category(&category)?,
                    excerpt,
                })
            })
            .collect()
        })
        .await
    }

    async fn call<T, F>(&self, operation: F) -> Result<T, HarnessError>
    where
        T: Send + 'static,
        F: FnOnce(&mut rusqlite::Connection) -> Result<T, HarnessError> + Send + 'static,
    {
        self.connection.call(operation).await.map_err(worker_error)
    }
}

struct IndexedDocument {
    session_id: String,
    workspace_id: String,
    seq: u64,
    run_id: String,
    occurred_at_ms: u64,
    category: String,
    body: String,
}

fn indexed_document(session: &LocalSession, event: &SessionEvent) -> IndexedDocument {
    IndexedDocument {
        session_id: session.identity.session_id.as_str().to_owned(),
        workspace_id: session.workspace_id.as_str().to_owned(),
        seq: event.seq,
        run_id: event.run_id.as_str().to_owned(),
        occurred_at_ms: event.occurred_at_ms,
        category: category_name(event_category(&event.kind)).to_owned(),
        body: searchable_event_text(event),
    }
}

#[must_use]
pub fn event_category(kind: &ternilo_protocol::SessionEventKind) -> SessionEventCategory {
    use ternilo_protocol::SessionEventKind;
    match kind {
        SessionEventKind::UserMessage { .. } => SessionEventCategory::User,
        SessionEventKind::AssistantMessageDelta { .. }
        | SessionEventKind::AssistantReasoningDelta { .. }
        | SessionEventKind::AssistantMessage { .. } => SessionEventCategory::Assistant,
        SessionEventKind::ToolCallStarted { .. }
        | SessionEventKind::ToolCallFinished { .. }
        | SessionEventKind::JobUpdated { .. }
        | SessionEventKind::CodeDispatchStarted { .. }
        | SessionEventKind::CodeDispatchFinished { .. }
        | SessionEventKind::RuntimeExtensionChanged { .. } => SessionEventCategory::Tool,
        SessionEventKind::PlanUpdated { .. }
        | SessionEventKind::PlanReviewCompleted { .. }
        | SessionEventKind::TodoUpdated { .. }
        | SessionEventKind::GoalUpdated { .. }
        | SessionEventKind::GoalRoundStarted { .. }
        | SessionEventKind::ScheduleChanged { .. }
        | SessionEventKind::SubagentUpdated { .. }
        | SessionEventKind::WorkflowRunStarted { .. }
        | SessionEventKind::WorkflowPhaseChanged { .. }
        | SessionEventKind::WorkflowLogEmitted { .. }
        | SessionEventKind::WorkflowAgentStarted { .. }
        | SessionEventKind::WorkflowAgentFinished { .. }
        | SessionEventKind::WorkflowRunFinished { .. } => SessionEventCategory::Planning,
        SessionEventKind::ContextCompactionStarted { .. }
        | SessionEventKind::ContextCompacted { .. } => SessionEventCategory::Compaction,
        SessionEventKind::DeliverableProduced { .. } => SessionEventCategory::Deliverable,
        SessionEventKind::CommandStarted { .. }
        | SessionEventKind::FeedbackSubmitted { .. }
        | SessionEventKind::CommandFinished { .. }
        | SessionEventKind::FeedbackRecorded { .. }
        | SessionEventKind::UserQuestionAsked { .. }
        | SessionEventKind::UserQuestionAnswered { .. }
        | SessionEventKind::HookResult { .. }
        | SessionEventKind::HookContextAdded { .. } => SessionEventCategory::Interaction,
        SessionEventKind::TurnFailed { .. } | SessionEventKind::TurnCancelled => {
            SessionEventCategory::Error
        }
        SessionEventKind::TurnStarted
        | SessionEventKind::ExecutionActivityChanged { .. }
        | SessionEventKind::WorkspaceExecutionWaiting
        | SessionEventKind::WorkspaceExecutionAcquired
        | SessionEventKind::StepStarted { .. }
        | SessionEventKind::ModelRequestStarted { .. }
        | SessionEventKind::ProviderUsageStarted { .. }
        | SessionEventKind::ProviderUsageFinished { .. }
        | SessionEventKind::ModelRetryScheduled { .. }
        | SessionEventKind::ModelRetryStarted { .. }
        | SessionEventKind::ModelRetryCancelled { .. }
        | SessionEventKind::StepFinished { .. }
        | SessionEventKind::SessionTitleGenerationStarted
        | SessionEventKind::SessionTitleGenerated { .. }
        | SessionEventKind::SessionTitleGenerationFinished { .. }
        | SessionEventKind::TurnFinished { .. } => SessionEventCategory::Lifecycle,
    }
}

#[must_use]
pub fn searchable_event_text(event: &SessionEvent) -> String {
    use ternilo_protocol::SessionEventKind;
    match &event.kind {
        SessionEventKind::UserMessage { content, .. } => content.clone(),
        SessionEventKind::AssistantMessageDelta { delta, .. }
        | SessionEventKind::AssistantReasoningDelta { delta, .. } => delta.clone(),
        SessionEventKind::AssistantMessage { response, .. } => [
            response.reasoning_content.as_deref().unwrap_or_default(),
            response.content.as_str(),
        ]
        .into_iter()
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n"),
        SessionEventKind::ToolCallStarted { call } => format!("{} {}", call.name, call.arguments),
        SessionEventKind::ToolCallFinished { name, output, .. } => {
            format!("{name} {}", output.content)
        }
        SessionEventKind::JobUpdated { job } => format!(
            "background job {} {} {:?} {}",
            job.job_id,
            job.command,
            job.status,
            job.error.as_deref().unwrap_or_default()
        ),
        SessionEventKind::CodeDispatchStarted {
            parent_call_id,
            call,
        } => format!(
            "code dispatch {parent_call_id} {} {}",
            call.name, call.arguments
        ),
        SessionEventKind::CodeDispatchFinished {
            parent_call_id,
            name,
            output,
            ..
        } => format!("code dispatch {parent_call_id} {name} {}", output.content),
        SessionEventKind::RuntimeExtensionChanged {
            package_id,
            version,
            action,
        } => format!("runtime extension {package_id} {version} {action:?}"),
        SessionEventKind::PlanUpdated { explanation, items } => format!(
            "{} {}",
            explanation.as_deref().unwrap_or_default(),
            items
                .iter()
                .map(|item| item.step.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        SessionEventKind::PlanReviewCompleted {
            plan,
            approved,
            feedback,
        } => format!(
            "plan review {} {} {}",
            if *approved { "approved" } else { "revision" },
            plan,
            feedback.as_deref().unwrap_or_default()
        ),
        SessionEventKind::TodoUpdated { items } => items
            .iter()
            .map(|item| item.step.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        SessionEventKind::GoalUpdated { objective, .. }
        | SessionEventKind::GoalRoundStarted { objective, .. } => objective.clone(),
        SessionEventKind::ContextCompacted { compaction, .. } => compaction.summary.clone(),
        SessionEventKind::DeliverableProduced {
            path, operation, ..
        } => {
            format!("{operation} {path}")
        }
        SessionEventKind::CommandStarted { command_name, .. } => command_name.clone(),
        SessionEventKind::FeedbackSubmitted { text, .. } => text.clone(),
        SessionEventKind::CommandFinished { outcome, .. } => outcome.code.clone(),
        SessionEventKind::HookResult { result } => format!(
            "{} {} {} {} {}",
            result.handler_id,
            result.dialect,
            result.point.wire_name(),
            result.reason.as_deref().unwrap_or_default(),
            result.stderr_summary.as_deref().unwrap_or_default()
        ),
        SessionEventKind::HookContextAdded {
            handler_id,
            dialect,
            content,
            ..
        } => format!("{handler_id} {dialect} {content}"),
        SessionEventKind::TurnFinished { answer, .. } => answer.clone(),
        SessionEventKind::SessionTitleGenerated { title } => title.clone(),
        SessionEventKind::TurnFailed { message } => message.clone(),
        _ => serde_json::to_string(&event.kind).unwrap_or_default(),
    }
}

fn literal_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

const fn category_name(category: SessionEventCategory) -> &'static str {
    match category {
        SessionEventCategory::User => "user",
        SessionEventCategory::Assistant => "assistant",
        SessionEventCategory::Tool => "tool",
        SessionEventCategory::Planning => "planning",
        SessionEventCategory::Compaction => "compaction",
        SessionEventCategory::Deliverable => "deliverable",
        SessionEventCategory::Interaction => "interaction",
        SessionEventCategory::Error => "error",
        SessionEventCategory::Lifecycle => "lifecycle",
        SessionEventCategory::Other => "other",
    }
}

fn parse_category(value: &str) -> Result<SessionEventCategory, HarnessError> {
    match value {
        "user" => Ok(SessionEventCategory::User),
        "assistant" => Ok(SessionEventCategory::Assistant),
        "tool" => Ok(SessionEventCategory::Tool),
        "planning" => Ok(SessionEventCategory::Planning),
        "compaction" => Ok(SessionEventCategory::Compaction),
        "deliverable" => Ok(SessionEventCategory::Deliverable),
        "interaction" => Ok(SessionEventCategory::Interaction),
        "error" => Ok(SessionEventCategory::Error),
        "lifecycle" => Ok(SessionEventCategory::Lifecycle),
        "other" => Ok(SessionEventCategory::Other),
        _ => Err(HarnessError::execution(format!(
            "unknown indexed session event category {value:?}"
        ))),
    }
}

fn to_i64(value: u64, label: &str) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::execution(format!("{label} exceeds SQLite integer range")))
}

fn from_i64(value: i64, label: &str) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution(format!("{label} is negative")))
}

#[allow(clippy::needless_pass_by_value)]
fn sqlite_error(error: rusqlite::Error) -> HarnessError {
    HarnessError::execution(format!("local session search SQLite error: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn worker_error(error: tokio_rusqlite::Error<HarnessError>) -> HarnessError {
    HarnessError::execution(format!("local session search worker error: {error}"))
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set private session search index permissions {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn set_private_permissions(_: &Path) -> Result<(), HarnessError> {
    Ok(())
}
