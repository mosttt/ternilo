use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_kernel::SessionProjectionUnit;
use ternilo_protocol::{HarnessError, RunId, SessionEvent, SessionProjectionSnapshot};
use tokio_rusqlite::{
    Connection,
    rusqlite::{OptionalExtension as _, params_from_iter},
};

use crate::{
    event_store::JsonlEventStore,
    state::{LocalSession, LocalState},
};

const CACHE_SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS session_projection_checkpoints (
    session_id TEXT PRIMARY KEY,
    lifecycle_created_at_ms INTEGER NOT NULL CHECK (lifecycle_created_at_ms >= 0),
    workspace_id TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 0),
    checkpoint_json TEXT NOT NULL
);
";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionCheckpoint {
    schema_version: u32,
    session_id: String,
    lifecycle_created_at_ms: u64,
    workspace_id: String,
    rows: BTreeMap<String, ProjectionCheckpointRow>,
    #[serde(default)]
    discarded_runs: BTreeSet<RunId>,
}

impl ProjectionCheckpoint {
    fn matches(&self, session: &LocalSession) -> bool {
        self.schema_version == CACHE_SCHEMA_VERSION
            && self.session_id == session.identity.session_id.as_str()
            && self.lifecycle_created_at_ms == session.created_at_ms
            && self.workspace_id == session.workspace_id.as_str()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionCheckpointRow {
    version: u32,
    through_seq: Option<u64>,
    state: Value,
}

struct ProjectionRegistry {
    units: Vec<Arc<dyn SessionProjectionUnit>>,
}

impl ProjectionRegistry {
    fn restore_floor(
        &self,
        session: &LocalSession,
        checkpoint: Option<&ProjectionCheckpoint>,
    ) -> u64 {
        let Some(checkpoint) = checkpoint.filter(|value| value.matches(session)) else {
            return 0;
        };
        self.units
            .iter()
            .map(|unit| {
                checkpoint
                    .rows
                    .get(unit.key())
                    .filter(|row| row.version == unit.version() && unit.valid_state(&row.state))
                    .map_or(0, |row| {
                        row.through_seq.map_or(0, |seq| seq.saturating_add(1))
                    })
            })
            .min()
            .unwrap_or(0)
    }

    fn restore(
        &self,
        session: &LocalSession,
        checkpoint: Option<&ProjectionCheckpoint>,
        events: &[SessionEvent],
        base_seq: u64,
        total_records: u64,
    ) -> Result<(SessionProjectionSnapshot, ProjectionCheckpoint), HarnessError> {
        let replaced = events
            .iter()
            .any(|event| event.regeneration_target().is_some());
        if replaced && base_seq > 0 {
            return Err(HarnessError::execution(
                "conversation replacement requires a full projection replay",
            ));
        }
        let checkpoint = checkpoint.filter(|value| value.matches(session) && !replaced);
        let active = ternilo_protocol::conversation_events(events);
        let mut discarded_runs = checkpoint
            .map(|value| value.discarded_runs.clone())
            .unwrap_or_default();
        if replaced {
            let retained: BTreeSet<_> = active.iter().map(|event| event.seq).collect();
            discarded_runs.extend(
                events
                    .iter()
                    .filter(|event| !retained.contains(&event.seq))
                    .map(|event| event.run_id.clone()),
            );
        }
        let as_of_seq = total_records.checked_sub(1);
        let mut values = BTreeMap::new();
        let mut rows = BTreeMap::new();

        for unit in &self.units {
            let cached = checkpoint
                .and_then(|value| value.rows.get(unit.key()))
                .filter(|row| {
                    row.version == unit.version()
                        && unit.valid_state(&row.state)
                        && row.through_seq.is_none_or(|seq| Some(seq) <= as_of_seq)
                });
            if cached.is_none() && base_seq > 0 {
                return Err(HarnessError::execution(format!(
                    "projection {:?} requires a full log replay",
                    unit.key()
                )));
            }
            let mut state = cached.map_or_else(|| unit.initial(), |row| row.state.clone());
            let through_seq = cached.and_then(|row| row.through_seq);
            for event in active.iter() {
                if (base_seq == 0 || !discarded_runs.contains(&event.run_id))
                    && through_seq.is_none_or(|seq| event.seq > seq)
                {
                    unit.apply(&mut state, event)?;
                }
            }
            values.insert(unit.key().to_owned(), unit.view(&state)?);
            rows.insert(
                unit.key().to_owned(),
                ProjectionCheckpointRow {
                    version: unit.version(),
                    through_seq: as_of_seq,
                    state,
                },
            );
        }

        Ok((
            SessionProjectionSnapshot {
                session_id: session.identity.session_id.clone(),
                as_of_seq,
                values,
            },
            ProjectionCheckpoint {
                schema_version: CACHE_SCHEMA_VERSION,
                session_id: session.identity.session_id.as_str().to_owned(),
                lifecycle_created_at_ms: session.created_at_ms,
                workspace_id: session.workspace_id.as_str().to_owned(),
                rows,
                discarded_runs,
            },
        ))
    }
}

pub(crate) struct LocalProjectionCache {
    connection: Connection,
}

impl LocalProjectionCache {
    pub(crate) async fn open(path: &Path) -> Result<Self, HarnessError> {
        let connection = Connection::open(path).await.map_err(|error| {
            HarnessError::execution(format!(
                "open local session projection cache {}: {error}",
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
                database.execute_batch(SCHEMA).map_err(sqlite_error)
            })
            .await
            .map_err(worker_error)?;
        Ok(Self { connection })
    }

    pub(crate) async fn snapshot(
        &self,
        state: &LocalState,
        session: &LocalSession,
        units: Vec<Arc<dyn SessionProjectionUnit>>,
    ) -> Result<SessionProjectionSnapshot, HarnessError> {
        let registry = ProjectionRegistry { units };
        // Cache failures never make the read model unavailable: fall back to
        // the canonical log and let the best-effort write below self-heal it.
        let checkpoint = self.read(session).await.ok().flatten();
        let mut floor = registry.restore_floor(session, checkpoint.as_ref());
        let store = JsonlEventStore::new(&state.sessions_dir(), &session.identity.session_id);
        let mut tail = store.load_from(floor).await?;
        if tail.total_records < floor {
            floor = 0;
            tail = store.load_from(0).await?;
        }
        let restored = registry.restore(
            session,
            checkpoint.as_ref(),
            &tail.events,
            floor,
            tail.total_records,
        );
        let (snapshot, next) = match restored {
            Ok(value) => value,
            Err(_) if floor > 0 => {
                let full = store.load_from(0).await?;
                registry.restore(session, None, &full.events, 0, full.total_records)?
            }
            Err(error) => return Err(error),
        };
        let _ = self.write(next).await;
        Ok(snapshot)
    }

    pub(crate) async fn delete_fail_soft(&self, session_id: String) {
        let _ = self
            .call(move |database| {
                database
                    .execute(
                        "DELETE FROM session_projection_checkpoints WHERE session_id = ?1",
                        [session_id],
                    )
                    .map_err(sqlite_error)?;
                Ok(())
            })
            .await;
    }

    async fn read(
        &self,
        session: &LocalSession,
    ) -> Result<Option<ProjectionCheckpoint>, HarnessError> {
        let session_id = session.identity.session_id.as_str().to_owned();
        self.call(move |database| {
            let row = database
                .query_row(
                    "SELECT lifecycle_created_at_ms, workspace_id, schema_version, checkpoint_json
                     FROM session_projection_checkpoints WHERE session_id = ?1",
                    [session_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some((created_at_ms, workspace_id, schema_version, json)) = row else {
                return Ok(None);
            };
            let mut checkpoint: ProjectionCheckpoint =
                serde_json::from_str(&json).map_err(|error| {
                    HarnessError::execution(format!("parse local projection checkpoint: {error}"))
                })?;
            checkpoint.lifecycle_created_at_ms = from_i64(created_at_ms, "projection lifecycle")?;
            checkpoint.workspace_id = workspace_id;
            checkpoint.schema_version = u32::try_from(schema_version).map_err(|_| {
                HarnessError::execution("projection schema version is outside u32 range")
            })?;
            Ok(Some(checkpoint))
        })
        .await
    }

    async fn write(&self, checkpoint: ProjectionCheckpoint) -> Result<(), HarnessError> {
        let json = serde_json::to_string(&checkpoint).map_err(|error| {
            HarnessError::execution(format!("serialize local projection checkpoint: {error}"))
        })?;
        let schema_version = i64::from(checkpoint.schema_version);
        let created_at_ms = to_i64(checkpoint.lifecycle_created_at_ms, "projection lifecycle")?;
        self.call(move |database| {
            database
                .execute(
                    "INSERT INTO session_projection_checkpoints
                     (session_id, lifecycle_created_at_ms, workspace_id, schema_version, checkpoint_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(session_id) DO UPDATE SET
                       lifecycle_created_at_ms=excluded.lifecycle_created_at_ms,
                       workspace_id=excluded.workspace_id,
                       schema_version=excluded.schema_version,
                       checkpoint_json=excluded.checkpoint_json",
                    params_from_iter([
                        tokio_rusqlite::rusqlite::types::Value::Text(checkpoint.session_id),
                        tokio_rusqlite::rusqlite::types::Value::Integer(created_at_ms),
                        tokio_rusqlite::rusqlite::types::Value::Text(checkpoint.workspace_id),
                        tokio_rusqlite::rusqlite::types::Value::Integer(schema_version),
                        tokio_rusqlite::rusqlite::types::Value::Text(json),
                    ]),
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
        .await
    }

    async fn call<T, F>(&self, operation: F) -> Result<T, HarnessError>
    where
        T: Send + 'static,
        F: FnOnce(&mut tokio_rusqlite::rusqlite::Connection) -> Result<T, HarnessError>
            + Send
            + 'static,
    {
        self.connection.call(operation).await.map_err(worker_error)
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
fn sqlite_error(error: tokio_rusqlite::rusqlite::Error) -> HarnessError {
    HarnessError::execution(format!("local session projection SQLite error: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn worker_error(error: tokio_rusqlite::Error<HarnessError>) -> HarnessError {
    HarnessError::execution(format!("local session projection worker error: {error}"))
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set private session projection cache permissions {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn set_private_permissions(_: &Path) -> Result<(), HarnessError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use ternilo_protocol::{
        AgentId, FeedbackRating, PermissionPreset, PlanItem, PlanItemStatus, RunId,
        SessionEventKind, SessionId, SessionIdentity, SessionMode, SessionStats, TenantId, UserId,
        WorkspaceId,
    };

    use super::*;
    use crate::state::ModelSelection;

    fn session() -> LocalSession {
        LocalSession {
            server_model: None,
            identity: SessionIdentity {
                tenant_id: TenantId::new("local"),
                user_id: UserId::new("user"),
                agent_id: AgentId::new("agent"),
                session_id: SessionId::new("session"),
            },
            workspace_id: WorkspaceId::new("workspace"),
            workspace_path: "/tmp/workspace".to_owned(),
            parent_session_id: None,
            subagent: None,
            title: "Projection test".to_owned(),
            archived_at_ms: None,
            blank: false,
            permissions: PermissionPreset::WorkspaceWrite,
            model: ModelSelection::ProfileDefault,
            agent_preset: "standard".to_owned(),
            preset_plugins: Vec::new(),
            profile_plugins: Vec::new(),
            mode: SessionMode::Execute,
            created_at_ms: 42,
            updated_at_ms: 42,
        }
    }

    fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            seq,
            occurred_at_ms: 1_000 + seq,
            run_id: RunId::new("run"),
            kind,
        }
    }

    fn registry() -> ProjectionRegistry {
        let catalog = crate::catalog().expect("local catalog");
        let units = catalog
            .projection_units(&crate::local_profile())
            .expect("profile projection units");
        ProjectionRegistry { units }
    }

    #[test]
    fn versioned_checkpoint_restores_only_the_log_tail() {
        let registry = registry();
        let session = session();
        let prefix = vec![
            event(0, SessionEventKind::TurnStarted),
            event(
                1,
                SessionEventKind::UserMessage {
                    provenance: None,
                    content: "hello".to_owned(),
                    display_content: None,
                    source: None,
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
            ),
            event(
                2,
                SessionEventKind::PlanUpdated {
                    explanation: Some("first cut".to_owned()),
                    items: vec![PlanItem {
                        step: "project".to_owned(),
                        status: PlanItemStatus::InProgress,
                    }],
                },
            ),
        ];
        let (_, checkpoint) = registry
            .restore(&session, None, &prefix, 0, 3)
            .expect("full projection");
        assert_eq!(registry.restore_floor(&session, Some(&checkpoint)), 3);

        let tail = [
            event(
                3,
                SessionEventKind::FeedbackRecorded {
                    target_seq: 2,
                    revision: 1,
                    rating: Some(FeedbackRating::Positive),
                    note: None,
                },
            ),
            event(
                4,
                SessionEventKind::TurnFinished {
                    answer: "done".to_owned(),
                    finish_reason: ternilo_protocol::TurnFinishReason::Completed,
                },
            ),
        ];
        let (snapshot, _) = registry
            .restore(&session, Some(&checkpoint), &tail, 3, 5)
            .expect("tail restore");
        assert_eq!(snapshot.as_of_seq, Some(4));
        let stats: SessionStats =
            serde_json::from_value(snapshot.values["stats"].clone()).expect("stats view");
        assert_eq!(stats.events, 5);
        assert_eq!(stats.turns, 1);
        assert_eq!(stats.completed_turns, 1);
        assert_eq!(snapshot.values["feedback"]["2"]["rating"], "positive");
        assert_eq!(snapshot.values["feedback"]["2"]["note"], Value::Null);
    }

    #[test]
    fn partial_batch_replacement_preserves_prefix_stats_and_ignores_late_old_events() {
        let user = |seq, content: &str| {
            event(
                seq,
                SessionEventKind::UserMessage {
                    provenance: None,
                    content: content.to_owned(),
                    display_content: None,
                    source: None,
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
            )
        };
        let mut replacement = user(5, "edited 12");
        replacement.run_id = RunId::new("replacement");
        if let SessionEventKind::UserMessage { source, .. } = &mut replacement.kind {
            *source = Some(ternilo_protocol::UserMessageSource::Submission {
                submission_id: ternilo_protocol::SubmissionId::new("replacement-input"),
                created_at_ms: 1005,
                delivery: ternilo_protocol::SubmissionDelivery::Queue,
                regenerate_from: Some(2),
                skill_name: None,
            });
        }
        let mut started = event(4, SessionEventKind::TurnStarted);
        started.run_id.clone_from(&replacement.run_id);
        let history = [
            event(0, SessionEventKind::TurnStarted),
            user(1, "1"),
            user(2, "12"),
            user(3, "3"),
            started,
            replacement,
        ];
        let registry = registry();
        let session = session();
        let (projected, checkpoint) = registry.restore(&session, None, &history, 0, 6).unwrap();
        assert_eq!(projected.values["stats"]["user_messages"], 2);
        let late = [event(6, SessionEventKind::TurnCancelled)];
        let (restored, _) = registry
            .restore(&session, Some(&checkpoint), &late, 6, 7)
            .unwrap();
        assert_eq!(restored.values["stats"]["user_messages"], 2);
        assert_eq!(restored.values["stats"]["cancelled_turns"], 0);
    }

    #[test]
    fn stale_version_or_lifecycle_forces_a_full_refold() {
        let registry = registry();
        let session = session();
        let events = [event(0, SessionEventKind::TurnStarted)];
        let (_, mut checkpoint) = registry
            .restore(&session, None, &events, 0, 1)
            .expect("full projection");
        checkpoint.rows.get_mut("stats").expect("stats row").version += 1;
        assert_eq!(registry.restore_floor(&session, Some(&checkpoint)), 0);

        checkpoint.rows.get_mut("stats").expect("stats row").version -= 1;
        checkpoint.lifecycle_created_at_ms += 1;
        assert_eq!(registry.restore_floor(&session, Some(&checkpoint)), 0);
    }
}
