#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use ternilo_protocol::{HarnessError, SessionEvent, SessionId};
use ternilo_transport::{
    CommandId, CommandReply, ExecutorCommand, ExecutorHello, ExecutorId, SessionCursor,
};
use tokio_rusqlite::{Connection, params, rusqlite::OptionalExtension};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportStoreStatus {
    pub executors: u64,
    pub commands: u64,
    pub events: u64,
}

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS relay_executors (
    executor_id TEXT PRIMARY KEY,
    hello_json TEXT NOT NULL,
    last_seen_at_ms INTEGER NOT NULL CHECK (last_seen_at_ms >= 0)
);

CREATE TABLE IF NOT EXISTS relay_executor_leases (
    executor_id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    fencing_token INTEGER NOT NULL CHECK (fencing_token > 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0)
);

CREATE TABLE IF NOT EXISTS relay_commands (
    command_id TEXT PRIMARY KEY,
    executor_id TEXT NOT NULL,
    command_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'inflight', 'completed')),
    issued_at_ms INTEGER NOT NULL CHECK (issued_at_ms >= 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > issued_at_ms),
    owner_id TEXT,
    fencing_token INTEGER,
    dispatch_lease_until_ms INTEGER,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    reply_json TEXT,
    completed_at_ms INTEGER
);

CREATE INDEX IF NOT EXISTS relay_commands_dispatch
ON relay_commands (executor_id, state, expires_at_ms, dispatch_lease_until_ms);

CREATE TABLE IF NOT EXISTS relay_events (
    executor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq >= 0),
    event_json TEXT NOT NULL,
    PRIMARY KEY (executor_id, session_id, seq)
);

CREATE TABLE IF NOT EXISTS relay_readiness (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    probe_count INTEGER NOT NULL CHECK (probe_count >= 0)
);

INSERT OR IGNORE INTO relay_readiness (singleton, probe_count) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS node_commands (
    command_id TEXT PRIMARY KEY,
    command_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running', 'completed')),
    started_at_ms INTEGER NOT NULL CHECK (started_at_ms >= 0),
    reply_json TEXT,
    completed_at_ms INTEGER
);

CREATE INDEX IF NOT EXISTS node_commands_completed
ON node_commands (state, completed_at_ms DESC);
";

#[derive(Clone)]
pub struct TransportStore {
    connection: Connection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutorLease {
    pub fencing_token: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeCommandClaim {
    Execute,
    Completed(CommandReply),
    Indeterminate,
    Conflict,
}

impl TransportStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, HarnessError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                HarnessError::execution(format!(
                    "create transport store directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let connection = Connection::open(path).await.map_err(|error| {
            HarnessError::execution(format!("open transport store {}: {error}", path.display()))
        })?;
        set_private_permissions(path).await?;
        Self::initialize(connection).await
    }

    pub async fn open_in_memory() -> Result<Self, HarnessError> {
        let connection = Connection::open_in_memory()
            .await
            .map_err(|error| HarnessError::execution(format!("open transport store: {error}")))?;
        Self::initialize(connection).await
    }

    async fn initialize(connection: Connection) -> Result<Self, HarnessError> {
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
                database
                    .pragma_update(None, "foreign_keys", "ON")
                    .map_err(sqlite_error)?;
                database.execute_batch(SCHEMA).map_err(sqlite_error)
            })
            .await
            .map_err(store_call_error)?;
        Ok(Self { connection })
    }

    /// Create a transactionally consistent SQLite backup while this store remains online.
    pub async fn backup_to(&self, destination: impl AsRef<Path>) -> Result<(), HarnessError> {
        let destination = destination.as_ref();
        if let Some(parent) = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                HarnessError::execution(format!(
                    "create transport backup directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let destination = PathBuf::from(destination);
        let backup_path = destination.clone();
        self.call(move |database| {
            database
                .backup(tokio_rusqlite::rusqlite::MAIN_DB, &backup_path, None)
                .map_err(sqlite_error)?;
            let backup =
                tokio_rusqlite::rusqlite::Connection::open(&backup_path).map_err(sqlite_error)?;
            backup
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     DELETE FROM relay_executor_leases;
                     UPDATE relay_commands
                     SET state = 'pending', owner_id = NULL, fencing_token = NULL,
                         dispatch_lease_until_ms = NULL
                     WHERE state = 'inflight';
                     COMMIT;",
                )
                .map_err(sqlite_error)?;
            let check = backup
                .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
                .map_err(sqlite_error)?;
            if check != "ok" {
                return Err(HarnessError::execution(format!(
                    "transport backup integrity check failed: {check}"
                )));
            }
            Ok(())
        })
        .await?;
        set_private_permissions(&destination).await
    }

    /// Exercise a read/write transaction and return low-cardinality store gauges.
    pub async fn status(&self) -> Result<TransportStoreStatus, HarnessError> {
        self.call(|database| {
            let check = database
                .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
                .map_err(sqlite_error)?;
            if check != "ok" {
                return Err(HarnessError::execution(format!(
                    "transport store integrity check failed: {check}"
                )));
            }
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let changed = transaction
                .execute(
                    "UPDATE relay_readiness SET probe_count = probe_count + 1 WHERE singleton = 1",
                    [],
                )
                .map_err(sqlite_error)?;
            if changed != 1 {
                return Err(HarnessError::execution(
                    "transport readiness probe row is missing",
                ));
            }
            transaction.commit().map_err(sqlite_error)?;
            let executors = count_rows(database, "relay_executors")?;
            let commands = count_rows(database, "relay_commands")?;
            let events = count_rows(database, "relay_events")?;
            Ok(TransportStoreStatus {
                executors,
                commands,
                events,
            })
        })
        .await
    }

    pub async fn register_executor(
        &self,
        hello: &ExecutorHello,
        last_seen_at_ms: u64,
    ) -> Result<(), HarnessError> {
        hello.validate()?;
        let executor_id = hello.executor_id.as_str().to_owned();
        let hello_json = encode(hello, "executor hello")?;
        let last_seen_at_ms = to_sql_integer(last_seen_at_ms, "executor last-seen timestamp")?;
        self.call(move |database| {
            database
                .execute(
                    "INSERT INTO relay_executors (executor_id, hello_json, last_seen_at_ms)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(executor_id) DO UPDATE SET
                         hello_json = excluded.hello_json,
                         last_seen_at_ms = excluded.last_seen_at_ms",
                    params![executor_id, hello_json, last_seen_at_ms],
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
        .await
    }

    pub async fn mark_executor_seen(
        &self,
        executor_id: &ExecutorId,
        last_seen_at_ms: u64,
    ) -> Result<(), HarnessError> {
        executor_id.validate()?;
        let executor_id = executor_id.as_str().to_owned();
        let last_seen_at_ms = to_sql_integer(last_seen_at_ms, "executor last-seen timestamp")?;
        self.call(move |database| {
            database
                .execute(
                    "UPDATE relay_executors SET last_seen_at_ms = ?2 WHERE executor_id = ?1",
                    params![executor_id, last_seen_at_ms],
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
        .await
    }

    pub async fn known_executors(&self) -> Result<Vec<ExecutorId>, HarnessError> {
        self.call(|database| {
            let mut statement = database
                .prepare("SELECT executor_id FROM relay_executors ORDER BY executor_id")
                .map_err(sqlite_error)?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sqlite_error)?
                .map(|row| row.map(ExecutorId::new).map_err(sqlite_error))
                .collect()
        })
        .await
    }

    pub async fn acquire_executor_lease(
        &self,
        executor_id: &ExecutorId,
        owner_id: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<Option<ExecutorLease>, HarnessError> {
        executor_id.validate()?;
        require_owner(owner_id)?;
        if ttl_ms == 0 {
            return Err(HarnessError::invalid("executor lease TTL must be positive"));
        }
        let executor_id = executor_id.as_str().to_owned();
        let owner_id = owner_id.to_owned();
        let now_ms = to_sql_integer(now_ms, "executor lease timestamp")?;
        let expires_at_ms = to_sql_integer(
            u64::try_from(now_ms)
                .unwrap_or_default()
                .saturating_add(ttl_ms),
            "executor lease expiry",
        )?;
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let existing = transaction
                .query_row(
                    "SELECT owner_id, fencing_token, expires_at_ms
                     FROM relay_executor_leases WHERE executor_id = ?1",
                    [&executor_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_error)?;
            let lease = match existing {
                None => {
                    transaction
                        .execute(
                            "INSERT INTO relay_executor_leases
                             (executor_id, owner_id, fencing_token, expires_at_ms)
                             VALUES (?1, ?2, 1, ?3)",
                            params![executor_id, owner_id, expires_at_ms],
                        )
                        .map_err(sqlite_error)?;
                    Some(ExecutorLease { fencing_token: 1 })
                }
                Some((current_owner, token, _)) if current_owner == owner_id => {
                    transaction
                        .execute(
                            "UPDATE relay_executor_leases SET expires_at_ms = ?3
                             WHERE executor_id = ?1 AND owner_id = ?2",
                            params![executor_id, owner_id, expires_at_ms],
                        )
                        .map_err(sqlite_error)?;
                    Some(ExecutorLease {
                        fencing_token: from_sql_integer(token, "executor fencing token")?,
                    })
                }
                Some((_, token, current_expiry)) if current_expiry <= now_ms => {
                    let token = token.checked_add(1).ok_or_else(|| {
                        HarnessError::execution("executor fencing token overflow")
                    })?;
                    transaction
                        .execute(
                            "UPDATE relay_executor_leases
                             SET owner_id = ?2, fencing_token = ?3, expires_at_ms = ?4
                             WHERE executor_id = ?1",
                            params![executor_id, owner_id, token, expires_at_ms],
                        )
                        .map_err(sqlite_error)?;
                    Some(ExecutorLease {
                        fencing_token: from_sql_integer(token, "executor fencing token")?,
                    })
                }
                Some(_) => None,
            };
            transaction.commit().map_err(sqlite_error)?;
            Ok(lease)
        })
        .await
    }

    pub async fn renew_executor_lease(
        &self,
        executor_id: &ExecutorId,
        owner_id: &str,
        fencing_token: u64,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<bool, HarnessError> {
        executor_id.validate()?;
        require_owner(owner_id)?;
        if ttl_ms == 0 {
            return Err(HarnessError::invalid("executor lease TTL must be positive"));
        }
        let executor_id = executor_id.as_str().to_owned();
        let owner_id = owner_id.to_owned();
        let fencing_token = to_sql_integer(fencing_token, "executor fencing token")?;
        let now = now_ms;
        let now_ms = to_sql_integer(now_ms, "executor lease timestamp")?;
        let expires_at_ms = to_sql_integer(now.saturating_add(ttl_ms), "executor lease expiry")?;
        self.call(move |database| {
            let changed = database
                .execute(
                    "UPDATE relay_executor_leases SET expires_at_ms = ?5
                     WHERE executor_id = ?1 AND owner_id = ?2 AND fencing_token = ?3
                       AND expires_at_ms >= ?4",
                    params![executor_id, owner_id, fencing_token, now_ms, expires_at_ms],
                )
                .map_err(sqlite_error)?;
            Ok(changed == 1)
        })
        .await
    }

    pub async fn release_executor_lease(
        &self,
        executor_id: &ExecutorId,
        owner_id: &str,
        fencing_token: u64,
    ) -> Result<(), HarnessError> {
        executor_id.validate()?;
        require_owner(owner_id)?;
        let executor_id = executor_id.as_str().to_owned();
        let owner_id = owner_id.to_owned();
        let fencing_token = to_sql_integer(fencing_token, "executor fencing token")?;
        self.call(move |database| {
            database
                .execute(
                    "DELETE FROM relay_executor_leases
                     WHERE executor_id = ?1 AND owner_id = ?2 AND fencing_token = ?3",
                    params![executor_id, owner_id, fencing_token],
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
        .await
    }

    pub async fn enqueue_command(
        &self,
        executor_id: &ExecutorId,
        command: &ExecutorCommand,
    ) -> Result<(), HarnessError> {
        executor_id.validate()?;
        command.validate(command.issued_at_ms)?;
        let executor_id = executor_id.as_str().to_owned();
        let command_id = command.command_id.as_str().to_owned();
        let command_json = encode(command, "executor command")?;
        let issued_at_ms = to_sql_integer(command.issued_at_ms, "command issue timestamp")?;
        let expires_at_ms = to_sql_integer(command.expires_at_ms, "command expiry timestamp")?;
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let existing = transaction
                .query_row(
                    "SELECT executor_id, command_json FROM relay_commands WHERE command_id = ?1",
                    [&command_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            match existing {
                Some((stored_executor, stored_command))
                    if stored_executor != executor_id || stored_command != command_json =>
                {
                    return Err(HarnessError::policy(
                        "command id was reused with different command content",
                    ));
                }
                Some(_) => {}
                None => {
                    transaction
                        .execute(
                            "INSERT INTO relay_commands
                             (command_id, executor_id, command_json, state, issued_at_ms, expires_at_ms)
                             VALUES (?1, ?2, ?3, 'pending', ?4, ?5)",
                            params![
                                command_id,
                                executor_id,
                                command_json,
                                issued_at_ms,
                                expires_at_ms
                            ],
                        )
                        .map_err(sqlite_error)?;
                }
            }
            transaction.commit().map_err(sqlite_error)
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn claim_commands(
        &self,
        executor_id: &ExecutorId,
        owner_id: &str,
        fencing_token: u64,
        now_ms: u64,
        dispatch_ttl_ms: u64,
        limit: u32,
    ) -> Result<Vec<ExecutorCommand>, HarnessError> {
        executor_id.validate()?;
        require_owner(owner_id)?;
        if dispatch_ttl_ms == 0 || limit == 0 {
            return Err(HarnessError::invalid(
                "dispatch lease TTL and command limit must be positive",
            ));
        }
        let executor_id = executor_id.as_str().to_owned();
        let owner_id = owner_id.to_owned();
        let fencing_token = to_sql_integer(fencing_token, "executor fencing token")?;
        let now = now_ms;
        let now_ms = to_sql_integer(now_ms, "command claim timestamp")?;
        let lease_until_ms = to_sql_integer(
            now.saturating_add(dispatch_ttl_ms),
            "command dispatch lease",
        )?;
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let lease_valid = transaction
                .query_row(
                    "SELECT 1 FROM relay_executor_leases
                     WHERE executor_id = ?1 AND owner_id = ?2 AND fencing_token = ?3
                       AND expires_at_ms > ?4",
                    params![executor_id, owner_id, fencing_token, now_ms],
                    |_| Ok(()),
                )
                .optional()
                .map_err(sqlite_error)?
                .is_some();
            if !lease_valid {
                return Err(HarnessError::policy(
                    "relay instance does not own the executor lease",
                ));
            }
            let rows = {
                let mut statement = transaction
                    .prepare(
                        "SELECT command_id, command_json FROM relay_commands
                         WHERE executor_id = ?1 AND expires_at_ms > ?2 AND
                           (state = 'pending' OR
                            (state = 'inflight' AND dispatch_lease_until_ms <= ?2))
                         ORDER BY issued_at_ms, command_id LIMIT ?3",
                    )
                    .map_err(sqlite_error)?;
                statement
                    .query_map(params![executor_id, now_ms, i64::from(limit)], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(sqlite_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sqlite_error)?
            };
            let mut commands = Vec::with_capacity(rows.len());
            for (command_id, command_json) in rows {
                let command = decode(&command_json, "executor command")?;
                transaction
                    .execute(
                        "UPDATE relay_commands SET
                            state = 'inflight', owner_id = ?2, fencing_token = ?3,
                            dispatch_lease_until_ms = ?4, attempt_count = attempt_count + 1
                         WHERE command_id = ?1",
                        params![command_id, owner_id, fencing_token, lease_until_ms],
                    )
                    .map_err(sqlite_error)?;
                commands.push(command);
            }
            transaction.commit().map_err(sqlite_error)?;
            Ok(commands)
        })
        .await
    }

    pub async fn release_inflight_commands(
        &self,
        executor_id: &ExecutorId,
        owner_id: &str,
        fencing_token: u64,
    ) -> Result<(), HarnessError> {
        executor_id.validate()?;
        require_owner(owner_id)?;
        let executor_id = executor_id.as_str().to_owned();
        let owner_id = owner_id.to_owned();
        let fencing_token = to_sql_integer(fencing_token, "executor fencing token")?;
        self.call(move |database| {
            database
                .execute(
                    "UPDATE relay_commands SET state = 'pending', owner_id = NULL,
                        fencing_token = NULL, dispatch_lease_until_ms = NULL
                     WHERE executor_id = ?1 AND state = 'inflight'
                       AND owner_id = ?2 AND fencing_token = ?3",
                    params![executor_id, owner_id, fencing_token],
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
        .await
    }

    pub async fn complete_command(
        &self,
        executor_id: &ExecutorId,
        owner_id: &str,
        fencing_token: u64,
        reply: &CommandReply,
    ) -> Result<bool, HarnessError> {
        executor_id.validate()?;
        require_owner(owner_id)?;
        reply.command_id.validate()?;
        let executor_id = executor_id.as_str().to_owned();
        let owner_id = owner_id.to_owned();
        let fencing_token = to_sql_integer(fencing_token, "executor fencing token")?;
        let command_id = reply.command_id.as_str().to_owned();
        let reply_json = encode(reply, "command reply")?;
        let completed_at_ms =
            to_sql_integer(reply.completed_at_ms, "command completion timestamp")?;
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let row = transaction
                .query_row(
                    "SELECT executor_id, state, owner_id, fencing_token, reply_json
                     FROM relay_commands WHERE command_id = ?1",
                    [&command_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_error)?
                .ok_or_else(|| HarnessError::invalid("reply references an unknown command"))?;
            if row.0 != executor_id {
                return Err(HarnessError::policy(
                    "executor replied to another executor's command",
                ));
            }
            if row.1 == "completed" {
                if row.4.as_deref() != Some(reply_json.as_str()) {
                    return Err(HarnessError::policy(
                        "executor changed an already completed command reply",
                    ));
                }
                transaction.commit().map_err(sqlite_error)?;
                return Ok(false);
            }
            if row.1 != "inflight"
                || row.2.as_deref() != Some(owner_id.as_str())
                || row.3 != Some(fencing_token)
            {
                return Err(HarnessError::policy(
                    "stale relay ownership attempted to complete a command",
                ));
            }
            transaction
                .execute(
                    "UPDATE relay_commands SET state = 'completed', reply_json = ?2,
                        completed_at_ms = ?3, dispatch_lease_until_ms = NULL
                     WHERE command_id = ?1",
                    params![command_id, reply_json, completed_at_ms],
                )
                .map_err(sqlite_error)?;
            transaction.commit().map_err(sqlite_error)?;
            Ok(true)
        })
        .await
    }

    pub async fn command_reply(
        &self,
        command_id: &CommandId,
    ) -> Result<Option<CommandReply>, HarnessError> {
        command_id.validate()?;
        let command_id = command_id.as_str().to_owned();
        self.call(move |database| {
            let reply = database
                .query_row(
                    "SELECT reply_json FROM relay_commands
                     WHERE command_id = ?1 AND state = 'completed'",
                    [&command_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            reply
                .map(|reply| decode(&reply, "command reply"))
                .transpose()
        })
        .await
    }

    pub async fn command_exists(&self, command_id: &CommandId) -> Result<bool, HarnessError> {
        command_id.validate()?;
        let command_id = command_id.as_str().to_owned();
        self.call(move |database| {
            database
                .query_row(
                    "SELECT 1 FROM relay_commands WHERE command_id = ?1",
                    [&command_id],
                    |_| Ok(()),
                )
                .optional()
                .map(|row| row.is_some())
                .map_err(sqlite_error)
        })
        .await
    }

    pub async fn expire_commands(&self, now_ms: u64) -> Result<Vec<CommandReply>, HarnessError> {
        let now = now_ms;
        let now_ms = to_sql_integer(now_ms, "command expiry sweep timestamp")?;
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let command_ids = {
                let mut statement = transaction
                    .prepare(
                        "SELECT command_id FROM relay_commands
                         WHERE state != 'completed' AND expires_at_ms <= ?1
                         ORDER BY issued_at_ms, command_id",
                    )
                    .map_err(sqlite_error)?;
                statement
                    .query_map([now_ms], |row| row.get::<_, String>(0))
                    .map_err(sqlite_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sqlite_error)?
            };
            let mut replies = Vec::with_capacity(command_ids.len());
            for command_id in command_ids {
                let reply = CommandReply::failure(
                    CommandId::new(&command_id),
                    now,
                    HarnessError::policy("executor command expired before completion"),
                );
                let reply_json = encode(&reply, "expired command reply")?;
                transaction
                    .execute(
                        "UPDATE relay_commands SET state = 'completed', reply_json = ?2,
                            completed_at_ms = ?3, dispatch_lease_until_ms = NULL
                         WHERE command_id = ?1",
                        params![command_id, reply_json, now_ms],
                    )
                    .map_err(sqlite_error)?;
                replies.push(reply);
            }
            transaction.commit().map_err(sqlite_error)?;
            Ok(replies)
        })
        .await
    }

    pub async fn event_cursors(
        &self,
        executor_id: &ExecutorId,
    ) -> Result<Vec<SessionCursor>, HarnessError> {
        executor_id.validate()?;
        let executor_id = executor_id.as_str().to_owned();
        self.call(move |database| {
            let mut statement = database
                .prepare(
                    "SELECT session_id, MAX(seq) FROM relay_events
                     WHERE executor_id = ?1 GROUP BY session_id ORDER BY session_id",
                )
                .map_err(sqlite_error)?;
            let rows = statement
                .query_map([executor_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?;
            rows.into_iter()
                .map(|(session_id, sequence)| {
                    Ok(SessionCursor {
                        session_id: SessionId::new(session_id),
                        last_seq: Some(from_sql_integer(sequence, "session event sequence")?),
                    })
                })
                .collect()
        })
        .await
    }

    pub async fn merge_events(
        &self,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        events: &[SessionEvent],
    ) -> Result<(), HarnessError> {
        executor_id.validate()?;
        session_id.validate()?;
        let executor_id = executor_id.as_str().to_owned();
        let session_id = session_id.as_str().to_owned();
        let events = events.to_vec();
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let last_sequence = transaction
                .query_row(
                    "SELECT MAX(seq) FROM relay_events
                     WHERE executor_id = ?1 AND session_id = ?2",
                    params![executor_id, session_id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(sqlite_error)?;
            let mut expected = last_sequence.map_or(0, |sequence| sequence.saturating_add(1));
            for event in events {
                let sequence = to_sql_integer(event.seq, "session event sequence")?;
                let event_json = encode(&event, "session event")?;
                let existing = transaction
                    .query_row(
                        "SELECT event_json FROM relay_events
                         WHERE executor_id = ?1 AND session_id = ?2 AND seq = ?3",
                        params![executor_id, session_id, sequence],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                if let Some(existing) = existing {
                    let existing: SessionEvent = decode(&existing, "stored session event")?;
                    if existing != event {
                        return Err(HarnessError::policy(format!(
                            "executor changed existing session event {}",
                            event.seq
                        )));
                    }
                    continue;
                }
                if sequence != expected {
                    return Err(HarnessError::invalid(format!(
                        "executor event stream has a gap: expected {expected}, found {sequence}"
                    )));
                }
                transaction
                    .execute(
                        "INSERT INTO relay_events
                         (executor_id, session_id, seq, event_json) VALUES (?1, ?2, ?3, ?4)",
                        params![executor_id, session_id, sequence, event_json],
                    )
                    .map_err(sqlite_error)?;
                expected = expected.saturating_add(1);
            }
            transaction.commit().map_err(sqlite_error)
        })
        .await
    }

    pub async fn events(
        &self,
        executor_id: &ExecutorId,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        executor_id.validate()?;
        session_id.validate()?;
        let executor_id = executor_id.as_str().to_owned();
        let session_id = session_id.as_str().to_owned();
        self.call(move |database| {
            let mut statement = database
                .prepare(
                    "SELECT event_json FROM relay_events
                     WHERE executor_id = ?1 AND session_id = ?2 ORDER BY seq",
                )
                .map_err(sqlite_error)?;
            let rows = statement
                .query_map(params![executor_id, session_id], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?;
            rows.into_iter()
                .map(|event| decode(&event, "session event"))
                .collect()
        })
        .await
    }

    pub async fn delete_events(
        &self,
        executor_id: &ExecutorId,
        session_id: &SessionId,
    ) -> Result<(), HarnessError> {
        executor_id.validate()?;
        session_id.validate()?;
        let executor_id = executor_id.as_str().to_owned();
        let session_id = session_id.as_str().to_owned();
        self.call(move |database| {
            database
                .execute(
                    "DELETE FROM relay_events WHERE executor_id = ?1 AND session_id = ?2",
                    params![executor_id, session_id],
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
        .await
    }

    pub async fn claim_node_command(
        &self,
        command: &ExecutorCommand,
        started_at_ms: u64,
    ) -> Result<NodeCommandClaim, HarnessError> {
        command.command_id.validate()?;
        let command_id = command.command_id.as_str().to_owned();
        let command_json = encode(command, "node command")?;
        let started_at_ms = to_sql_integer(started_at_ms, "node command start timestamp")?;
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let row = transaction
                .query_row(
                    "SELECT command_json, state, reply_json FROM node_commands
                     WHERE command_id = ?1",
                    [&command_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_error)?;
            let claim = match row {
                None => {
                    transaction
                        .execute(
                            "INSERT INTO node_commands
                             (command_id, command_json, state, started_at_ms)
                             VALUES (?1, ?2, 'running', ?3)",
                            params![command_id, command_json, started_at_ms],
                        )
                        .map_err(sqlite_error)?;
                    NodeCommandClaim::Execute
                }
                Some((stored_command, _, _)) if stored_command != command_json => {
                    NodeCommandClaim::Conflict
                }
                Some((_, state, Some(reply))) if state == "completed" => {
                    NodeCommandClaim::Completed(decode(&reply, "node command reply")?)
                }
                Some((_, state, _)) if state == "running" => NodeCommandClaim::Indeterminate,
                Some(_) => {
                    return Err(HarnessError::execution(
                        "node command ledger contains an invalid state",
                    ));
                }
            };
            transaction.commit().map_err(sqlite_error)?;
            Ok(claim)
        })
        .await
    }

    pub async fn complete_node_command(
        &self,
        command: &ExecutorCommand,
        reply: &CommandReply,
        retain_completed: u32,
    ) -> Result<(), HarnessError> {
        if command.command_id != reply.command_id {
            return Err(HarnessError::invalid(
                "node command reply id does not match its command",
            ));
        }
        if retain_completed == 0 {
            return Err(HarnessError::invalid(
                "node command retention must be positive",
            ));
        }
        let command_id = command.command_id.as_str().to_owned();
        let command_json = encode(command, "node command")?;
        let reply_json = encode(reply, "node command reply")?;
        let completed_at_ms = to_sql_integer(reply.completed_at_ms, "node command completion")?;
        self.call(move |database| {
            let transaction = database
                .transaction_with_behavior(tokio_rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let stored_command = transaction
                .query_row(
                    "SELECT command_json FROM node_commands WHERE command_id = ?1",
                    [&command_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?
                .ok_or_else(|| HarnessError::invalid("complete unknown node command"))?;
            if stored_command != command_json {
                return Err(HarnessError::policy(
                    "node command id was reused with different content",
                ));
            }
            transaction
                .execute(
                    "UPDATE node_commands SET state = 'completed', reply_json = ?2,
                        completed_at_ms = ?3 WHERE command_id = ?1",
                    params![command_id, reply_json, completed_at_ms],
                )
                .map_err(sqlite_error)?;
            transaction
                .execute(
                    "DELETE FROM node_commands WHERE command_id IN (
                        SELECT command_id FROM node_commands WHERE state = 'completed'
                        ORDER BY completed_at_ms DESC, command_id DESC LIMIT -1 OFFSET ?1
                     )",
                    [i64::from(retain_completed)],
                )
                .map_err(sqlite_error)?;
            transaction.commit().map_err(sqlite_error)
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
        self.connection
            .call(operation)
            .await
            .map_err(store_call_error)
    }
}

fn require_owner(owner_id: &str) -> Result<(), HarnessError> {
    if owner_id.trim().is_empty() || owner_id.len() > 128 {
        Err(HarnessError::invalid(
            "relay owner id must contain 1 to 128 bytes",
        ))
    } else {
        Ok(())
    }
}

fn encode(value: &impl serde::Serialize, label: &str) -> Result<String, HarnessError> {
    serde_json::to_string(value)
        .map_err(|error| HarnessError::execution(format!("encode {label}: {error}")))
}

fn decode<T: serde::de::DeserializeOwned>(value: &str, label: &str) -> Result<T, HarnessError> {
    serde_json::from_str(value)
        .map_err(|error| HarnessError::execution(format!("decode {label}: {error}")))
}

#[allow(clippy::needless_pass_by_value)]
fn sqlite_error(error: tokio_rusqlite::rusqlite::Error) -> HarnessError {
    HarnessError::execution(format!("transport store SQLite error: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn store_call_error(error: tokio_rusqlite::Error<HarnessError>) -> HarnessError {
    HarnessError::execution(format!("transport store worker error: {error}"))
}

fn to_sql_integer(value: u64, label: &str) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::execution(format!("{label} exceeds SQLite integer range")))
}

fn from_sql_integer(value: i64, label: &str) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution(format!("{label} is negative")))
}

fn count_rows(
    database: &tokio_rusqlite::rusqlite::Connection,
    table: &str,
) -> Result<u64, HarnessError> {
    let count = database
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(sqlite_error)?;
    from_sql_integer(count, table)
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set transport store permissions on {}: {error}",
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
    use std::collections::BTreeSet;

    use serde_json::json;
    use ternilo_protocol::{RunId, SessionEventKind, TenantId, UserId};
    use ternilo_transport::{
        ApplicationOperation, EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorCommandBody,
        ExecutorKind, ExecutorScope,
    };

    use super::*;

    fn hello(executor_id: &str) -> ExecutorHello {
        ExecutorHello {
            protocol_version: EXECUTOR_PROTOCOL_VERSION,
            executor_id: ExecutorId::new(executor_id),
            executor_kind: ExecutorKind::EdgeNode,
            instance_nonce: "instance".to_owned(),
            catalog_revision: "catalog".to_owned(),
            capabilities: BTreeSet::from([ExecutorCapability::ApplicationRpc]),
        }
    }

    fn command(id: &str, issued_at_ms: u64) -> ExecutorCommand {
        ExecutorCommand {
            command_id: CommandId::new(id),
            input_provenance: None,
            input_authorization: None,
            scope: ExecutorScope {
                tenant_id: TenantId::new("tenant"),
                user_id: UserId::new("user"),
            },
            issued_at_ms,
            expires_at_ms: issued_at_ms + 1_000,
            body: ExecutorCommandBody::Application {
                request: ApplicationOperation::Snapshot,
            },
        }
    }

    #[tokio::test]
    async fn lease_fencing_prevents_a_stale_owner_from_claiming_commands() {
        let store = TransportStore::open_in_memory().await.unwrap();
        let executor = ExecutorId::new("home");
        store.register_executor(&hello("home"), 10).await.unwrap();
        let first = store
            .acquire_executor_lease(&executor, "relay-a", 10, 20)
            .await
            .unwrap()
            .unwrap();
        store
            .enqueue_command(&executor, &command("command-1", 10))
            .await
            .unwrap();
        assert!(
            store
                .acquire_executor_lease(&executor, "relay-b", 20, 20)
                .await
                .unwrap()
                .is_none()
        );
        let second = store
            .acquire_executor_lease(&executor, "relay-b", 31, 20)
            .await
            .unwrap()
            .unwrap();
        assert!(second.fencing_token > first.fencing_token);
        assert!(
            store
                .claim_commands(&executor, "relay-a", first.fencing_token, 31, 10, 1)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .claim_commands(&executor, "relay-b", second.fencing_token, 31, 10, 1)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn event_log_is_contiguous_idempotent_and_durable_in_the_store() {
        let store = TransportStore::open_in_memory().await.unwrap();
        let executor = ExecutorId::new("home");
        let session = SessionId::new("session");
        let event = SessionEvent {
            seq: 0,
            occurred_at_ms: 10,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnStarted,
        };
        store
            .merge_events(&executor, &session, std::slice::from_ref(&event))
            .await
            .unwrap();
        store
            .merge_events(&executor, &session, std::slice::from_ref(&event))
            .await
            .unwrap();
        assert_eq!(
            store.events(&executor, &session).await.unwrap(),
            vec![event.clone()]
        );

        let response = SessionEvent {
            seq: 1,
            occurred_at_ms: 20,
            run_id: RunId::new("run"),
            kind: SessionEventKind::AssistantMessage {
                step: 1,
                response: ternilo_protocol::ModelResponse {
                    provider: "openai".to_owned(),
                    model: "gpt-test".to_owned(),
                    content: "partial".to_owned(),
                    reasoning_content: None,
                    provider_state: None,
                    tool_calls: Vec::new(),
                    usage: Some(ternilo_protocol::ModelUsage {
                        input_tokens: 20,
                        output_tokens: 7,
                        cached_input_tokens: 5,
                        cache_write_tokens: Some(3),
                        reasoning_tokens: 2,
                    }),
                    finish_reason: ternilo_protocol::ModelFinishReason::MaxTokens,
                    provider_request_id: None,
                    attempts: 1,
                    request_digest: None,
                    replayed: false,
                },
            },
        };
        let finished = SessionEvent {
            seq: 2,
            occurred_at_ms: 30,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnFinished {
                answer: "partial".to_owned(),
                finish_reason: ternilo_protocol::TurnFinishReason::MaxTokens,
            },
        };
        store
            .merge_events(&executor, &session, &[response.clone(), finished.clone()])
            .await
            .unwrap();
        assert_eq!(
            store.events(&executor, &session).await.unwrap(),
            vec![event, response, finished]
        );

        let gap = SessionEvent {
            seq: 4,
            occurred_at_ms: 40,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnFinished {
                answer: "gap".to_owned(),
                finish_reason: ternilo_protocol::TurnFinishReason::Completed,
            },
        };
        assert!(
            store
                .merge_events(&executor, &session, &[gap])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn node_ledger_returns_completed_reply_and_marks_crash_as_indeterminate() {
        let store = TransportStore::open_in_memory().await.unwrap();
        let first = command("command-1", 10);
        assert_eq!(
            store.claim_node_command(&first, 10).await.unwrap(),
            NodeCommandClaim::Execute
        );
        assert_eq!(
            store.claim_node_command(&first, 11).await.unwrap(),
            NodeCommandClaim::Indeterminate
        );
        let reply = CommandReply::success(first.command_id.clone(), 12, json!({ "ok": true }));
        store
            .complete_node_command(&first, &reply, 10)
            .await
            .unwrap();
        assert_eq!(
            store.claim_node_command(&first, 13).await.unwrap(),
            NodeCommandClaim::Completed(reply)
        );
    }

    #[tokio::test]
    async fn event_and_node_reply_survive_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transport.sqlite3");
        let executor = ExecutorId::new("home");
        let session = SessionId::new("session");
        let event = SessionEvent {
            seq: 0,
            occurred_at_ms: 10,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnStarted,
        };
        let command = command("command-1", 10);
        let reply = CommandReply::success(command.command_id.clone(), 12, json!({ "ok": true }));
        {
            let store = TransportStore::open(&path).await.unwrap();
            store
                .merge_events(&executor, &session, std::slice::from_ref(&event))
                .await
                .unwrap();
            assert_eq!(
                store.claim_node_command(&command, 10).await.unwrap(),
                NodeCommandClaim::Execute
            );
            store
                .complete_node_command(&command, &reply, 10)
                .await
                .unwrap();
        }

        let reopened = TransportStore::open(path).await.unwrap();
        assert_eq!(
            reopened.events(&executor, &session).await.unwrap(),
            vec![event]
        );
        assert_eq!(
            reopened.claim_node_command(&command, 13).await.unwrap(),
            NodeCommandClaim::Completed(reply)
        );
    }

    #[tokio::test]
    async fn online_backup_is_consistent_and_reopens_without_wal_files() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite3");
        let backup_path = directory.path().join("backup.sqlite3");
        let executor = ExecutorId::new("home");
        let session = SessionId::new("session");
        let event = SessionEvent {
            seq: 0,
            occurred_at_ms: 10,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnStarted,
        };
        let store = TransportStore::open(&source_path).await.unwrap();
        store.register_executor(&hello("home"), 10).await.unwrap();
        store
            .acquire_executor_lease(&executor, "old-relay", 10, 60_000)
            .await
            .unwrap()
            .unwrap();
        store
            .merge_events(&executor, &session, std::slice::from_ref(&event))
            .await
            .unwrap();

        store.backup_to(&backup_path).await.unwrap();
        assert!(backup_path.is_file());
        assert!(!backup_path.with_extension("sqlite3-wal").exists());

        let restored = TransportStore::open(&backup_path).await.unwrap();
        assert_eq!(restored.known_executors().await.unwrap(), vec![executor]);
        assert!(
            restored
                .acquire_executor_lease(&ExecutorId::new("home"), "restored-relay", 11, 60_000)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            restored
                .events(&ExecutorId::new("home"), &session)
                .await
                .unwrap(),
            vec![event]
        );
        assert_eq!(
            restored.status().await.unwrap(),
            TransportStoreStatus {
                executors: 1,
                commands: 0,
                events: 1,
            }
        );
    }

    #[tokio::test]
    async fn readiness_status_commits_a_real_main_database_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transport.sqlite3");
        let store = TransportStore::open(&path).await.unwrap();
        store.status().await.unwrap();
        store.status().await.unwrap();

        let database = tokio_rusqlite::rusqlite::Connection::open(path).unwrap();
        let probe_count = database
            .query_row(
                "SELECT probe_count FROM relay_readiness WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(probe_count, 2);
    }
}
