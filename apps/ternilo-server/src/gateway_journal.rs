//! Durable Node delivery, shared by every server mode and database backend.

use sqlx::{Any, Row, Transaction};
use ternilo_control::EdgeStore;
use ternilo_protocol::{HarnessError, SessionEvent, SessionId, TenantId};
use ternilo_storage::{Database, lock};
use ternilo_transport::{
    AcceptedUploadBatch, ApplicationOperation, CommandId, CommandReply, ExecutorCommand,
    ExecutorCommandBody, ExecutorId, ExecutorScope,
};

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS gateway_leases (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL CHECK (fencing_token >= 0),
    expires_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id)
);
CREATE TABLE IF NOT EXISTS gateway_commands (
    tenant_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    command_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'inflight', 'completed')),
    issued_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    owner_id TEXT,
    fencing_token BIGINT,
    dispatch_until_ms BIGINT,
    reply_json TEXT,
    completed_at_ms BIGINT,
    PRIMARY KEY (tenant_id, command_id)
);
CREATE INDEX IF NOT EXISTS gateway_commands_dispatch
ON gateway_commands (tenant_id, executor_id, state, expires_at_ms, dispatch_until_ms);
";

const POSTGRES_SCHEMA: &str = r"
ALTER TABLE gateway_leases ENABLE ROW LEVEL SECURITY;
CREATE POLICY gateway_lease_scope ON gateway_leases
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
ALTER TABLE gateway_commands ENABLE ROW LEVEL SECURITY;
CREATE POLICY gateway_command_scope ON gateway_commands
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_leases, gateway_commands TO ternilo_runtime;
END IF;
END $$;
";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RouteKey {
    pub(crate) tenant_id: TenantId,
    pub(crate) executor_id: ExecutorId,
}

impl RouteKey {
    pub(crate) fn new(tenant_id: TenantId, executor_id: ExecutorId) -> Self {
        Self {
            tenant_id,
            executor_id,
        }
    }

    fn lock_key(&self) -> String {
        format!("gateway:{}:{}", self.tenant_id, self.executor_id)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GatewayLease {
    pub(crate) owner_id: String,
    pub(crate) fencing_token: u64,
}

pub(crate) struct GatewayJournal {
    database: Database,
}

// Directory locations and local metadata stay on the Node; task mutations remain durable.
pub(crate) fn requires_ephemeral_delivery(body: &ExecutorCommandBody) -> bool {
    let ExecutorCommandBody::Application { request } = body else {
        return false;
    };
    request.requires_ephemeral_delivery()
        || matches!(
            request,
            ApplicationOperation::Snapshot
                | ApplicationOperation::DirectoryList { .. }
                | ApplicationOperation::DirectoryCreate { .. }
                | ApplicationOperation::WorkspaceCreate { .. }
                | ApplicationOperation::SessionReferenceCandidates { .. }
                | ApplicationOperation::SessionSkills { .. }
                | ApplicationOperation::SessionSkillResolve { .. }
                | ApplicationOperation::SessionPlugins { .. }
                | ApplicationOperation::ExtensionInventory
        )
}

impl GatewayJournal {
    pub(crate) async fn open(database: Database) -> Result<Self, HarnessError> {
        database
            .initialize("gateway", 1, SCHEMA, POSTGRES_SCHEMA)
            .await?;
        Ok(Self { database })
    }

    async fn transaction(
        &self,
        route: &RouteKey,
    ) -> Result<Transaction<'static, Any>, HarnessError> {
        route.tenant_id.validate()?;
        route.executor_id.validate()?;
        let mut transaction = self.database.tenant_transaction(&route.tenant_id).await?;
        lock(&mut transaction, &route.lock_key()).await?;
        Ok(transaction)
    }

    pub(crate) async fn acquire(
        &self,
        route: &RouteKey,
        owner_id: &str,
        now: u64,
        ttl: u64,
    ) -> Result<Option<GatewayLease>, HarnessError> {
        let mut transaction = self.transaction(route).await?;
        sqlx::query(
            "INSERT INTO gateway_leases (tenant_id, executor_id, owner_id, fencing_token, expires_at_ms)
             VALUES ($1, $2, '', 0, 0) ON CONFLICT (tenant_id, executor_id) DO NOTHING",
        )
        .bind(route.tenant_id.as_str()).bind(route.executor_id.as_str())
        .execute(&mut *transaction).await.map_err(database_error)?;
        let row = sqlx::query(
            "SELECT owner_id, fencing_token, expires_at_ms FROM gateway_leases
             WHERE tenant_id = $1 AND executor_id = $2",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let current_owner: String = row.try_get("owner_id").map_err(database_error)?;
        let expires: i64 = row.try_get("expires_at_ms").map_err(database_error)?;
        if current_owner != owner_id && expires > integer(now)? {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        }
        let current: i64 = row.try_get("fencing_token").map_err(database_error)?;
        let next = current
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("gateway fencing token overflow"))?;
        sqlx::query(
            "UPDATE gateway_leases SET owner_id = $3, fencing_token = $4, expires_at_ms = $5
             WHERE tenant_id = $1 AND executor_id = $2",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(owner_id)
        .bind(next)
        .bind(integer(now.saturating_add(ttl))?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(Some(GatewayLease {
            owner_id: owner_id.to_owned(),
            fencing_token: unsigned(next)?,
        }))
    }

    pub(crate) async fn renew(
        &self,
        route: &RouteKey,
        lease: &GatewayLease,
        now: u64,
        ttl: u64,
    ) -> Result<bool, HarnessError> {
        let mut transaction = self.transaction(route).await?;
        let changed = sqlx::query(
            "UPDATE gateway_leases SET expires_at_ms = $6
             WHERE tenant_id = $1 AND executor_id = $2 AND owner_id = $3
               AND fencing_token = $4 AND expires_at_ms > $5",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(&lease.owner_id)
        .bind(integer(lease.fencing_token)?)
        .bind(integer(now)?)
        .bind(integer(now.saturating_add(ttl))?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        transaction.commit().await.map_err(database_error)?;
        Ok(changed == 1)
    }

    pub(crate) async fn release(
        &self,
        route: &RouteKey,
        lease: &GatewayLease,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.transaction(route).await?;
        sqlx::query(
            "UPDATE gateway_commands SET state = 'pending', owner_id = NULL,
                 fencing_token = NULL, dispatch_until_ms = NULL
             WHERE tenant_id = $1 AND executor_id = $2 AND state = 'inflight'
               AND owner_id = $3 AND fencing_token = $4",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(&lease.owner_id)
        .bind(integer(lease.fencing_token)?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // Keep the fencing counter across clean shutdown and reconnect.
        sqlx::query(
            "UPDATE gateway_leases SET expires_at_ms = 0
             WHERE tenant_id = $1 AND executor_id = $2 AND owner_id = $3 AND fencing_token = $4",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(&lease.owner_id)
        .bind(integer(lease.fencing_token)?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)
    }

    pub(crate) async fn enqueue(
        &self,
        route: &RouteKey,
        command: &ExecutorCommand,
    ) -> Result<(), HarnessError> {
        command.validate(command.issued_at_ms)?;
        if requires_ephemeral_delivery(&command.body) {
            return Err(HarnessError::policy(
                "private Node metadata requires ephemeral delivery",
            ));
        }
        if command.scope.tenant_id != route.tenant_id {
            return Err(HarnessError::policy(
                "command tenant differs from its gateway route",
            ));
        }
        let encoded = encode(command)?;
        let mut transaction = self.transaction(route).await?;
        sqlx::query(
            "INSERT INTO gateway_commands
             (tenant_id, command_id, executor_id, command_json, state, issued_at_ms, expires_at_ms)
             VALUES ($1, $2, $3, $4, 'pending', $5, $6)
             ON CONFLICT (tenant_id, command_id) DO NOTHING",
        )
        .bind(route.tenant_id.as_str())
        .bind(command.command_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(&encoded)
        .bind(integer(command.issued_at_ms)?)
        .bind(integer(command.expires_at_ms)?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let row = sqlx::query(
            "SELECT executor_id, command_json FROM gateway_commands WHERE tenant_id = $1 AND command_id = $2",
        )
        .bind(route.tenant_id.as_str()).bind(command.command_id.as_str())
        .fetch_one(&mut *transaction).await.map_err(database_error)?;
        if row
            .try_get::<String, _>("executor_id")
            .map_err(database_error)?
            != route.executor_id.as_str()
            || row
                .try_get::<String, _>("command_json")
                .map_err(database_error)?
                != encoded
        {
            return Err(HarnessError::policy(
                "command ID was reused with different content or target",
            ));
        }
        if let Some(provenance) = &command.input_provenance {
            let (session_id, target_subagent_id) = match &command.body {
                ExecutorCommandBody::Application { request } => match request {
                    ApplicationOperation::SessionSubmit { session_id, .. }
                    | ApplicationOperation::SessionTurn { session_id, .. }
                    | ApplicationOperation::SessionSkillTurn { session_id, .. } => {
                        (session_id, None)
                    }
                    ApplicationOperation::SessionSubagentFollowup {
                        session_id,
                        subagent_id,
                        ..
                    } => (session_id, Some(subagent_id)),
                    _ => return Err(HarnessError::invalid("command does not create an input")),
                },
                _ => return Err(HarnessError::invalid("command does not create an input")),
            };
            EdgeStore::record_input_provenance_in_transaction(
                &mut transaction,
                &route.tenant_id,
                &route.executor_id,
                session_id,
                target_subagent_id,
                provenance,
                command.issued_at_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)
    }

    pub(crate) async fn claim(
        &self,
        route: &RouteKey,
        lease: &GatewayLease,
        now: u64,
        ttl: u64,
        limit: u32,
    ) -> Result<Vec<ExecutorCommand>, HarnessError> {
        let mut transaction = self.transaction(route).await?;
        require_lease(&mut transaction, route, lease, now).await?;
        let rows = sqlx::query(
            "SELECT command_id, command_json FROM gateway_commands
             WHERE tenant_id = $1 AND executor_id = $2 AND expires_at_ms > $3
               AND (state = 'pending' OR (state = 'inflight' AND
                    (dispatch_until_ms <= $3 OR owner_id <> $4 OR fencing_token <> $5)))
             ORDER BY issued_at_ms, command_id LIMIT $6",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(integer(now)?)
        .bind(&lease.owner_id)
        .bind(integer(lease.fencing_token)?)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let mut commands = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("command_id").map_err(database_error)?;
            let encoded: String = row.try_get("command_json").map_err(database_error)?;
            sqlx::query(
                "UPDATE gateway_commands SET state = 'inflight', owner_id = $3,
                     fencing_token = $4, dispatch_until_ms = $5
                 WHERE tenant_id = $1 AND command_id = $2",
            )
            .bind(route.tenant_id.as_str())
            .bind(id)
            .bind(&lease.owner_id)
            .bind(integer(lease.fencing_token)?)
            .bind(integer(now.saturating_add(ttl))?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            commands.push(decode(&encoded)?);
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(commands)
    }

    pub(crate) async fn complete(
        &self,
        route: &RouteKey,
        lease: &GatewayLease,
        reply: &CommandReply,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.transaction(route).await?;
        require_lease(&mut transaction, route, lease, now).await?;
        let row = sqlx::query(
            "SELECT executor_id, state, owner_id, fencing_token, reply_json, command_json FROM gateway_commands
             WHERE tenant_id = $1 AND command_id = $2",
        )
        .bind(route.tenant_id.as_str())
        .bind(reply.command_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("reply references an unknown command"))?;
        if row
            .try_get::<String, _>("executor_id")
            .map_err(database_error)?
            != route.executor_id.as_str()
        {
            return Err(HarnessError::policy(
                "Node replied to another Node's command",
            ));
        }
        let command: ExecutorCommand = serde_json::from_str(
            &row.try_get::<String, _>("command_json")
                .map_err(database_error)?,
        )
        .map_err(|error| {
            HarnessError::execution(format!("decode accepted Node command: {error}"))
        })?;
        if let (
            Some(expected),
            ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionSubmit { .. },
            },
            ternilo_transport::CommandOutcome::Ok { value },
        ) = (&command.input_provenance, &command.body, &reply.outcome)
        {
            let submission: ternilo_protocol::SessionSubmission =
                serde_json::from_value(value.clone()).map_err(|error| {
                    HarnessError::invalid(format!("decode Node submission receipt: {error}"))
                })?;
            submission.validate()?;
            if submission.provenance.as_ref() != Some(expected) {
                return Err(HarnessError::policy(
                    "Node changed the accepted submission author",
                ));
            }
        }
        let encoded = encode(reply)?;
        let state: String = row.try_get("state").map_err(database_error)?;
        if state == "completed" {
            if row
                .try_get::<Option<String>, _>("reply_json")
                .map_err(database_error)?
                .as_deref()
                != Some(encoded.as_str())
            {
                return Err(HarnessError::policy(
                    "Node changed an already completed reply",
                ));
            }
        } else {
            if state != "inflight"
                || row
                    .try_get::<Option<String>, _>("owner_id")
                    .map_err(database_error)?
                    .as_deref()
                    != Some(lease.owner_id.as_str())
                || row
                    .try_get::<Option<i64>, _>("fencing_token")
                    .map_err(database_error)?
                    != Some(integer(lease.fencing_token)?)
            {
                return Err(HarnessError::policy(
                    "stale gateway ownership attempted to complete a command",
                ));
            }
            sqlx::query(
                "UPDATE gateway_commands SET state = 'completed', reply_json = $3,
                     completed_at_ms = $4, dispatch_until_ms = NULL
                 WHERE tenant_id = $1 AND command_id = $2",
            )
            .bind(route.tenant_id.as_str())
            .bind(reply.command_id.as_str())
            .bind(encoded)
            .bind(integer(reply.completed_at_ms)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)
    }

    pub(crate) async fn contains(
        &self,
        route: &RouteKey,
        id: &CommandId,
    ) -> Result<bool, HarnessError> {
        let mut transaction = self.database.tenant_transaction(&route.tenant_id).await?;
        let found = sqlx::query("SELECT command_id FROM gateway_commands WHERE tenant_id = $1 AND executor_id = $2 AND command_id = $3")
            .bind(route.tenant_id.as_str()).bind(route.executor_id.as_str()).bind(id.as_str())
            .fetch_optional(&mut *transaction).await.map_err(database_error)?.is_some();
        transaction.commit().await.map_err(database_error)?;
        Ok(found)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn begin_upload_sync(
        &self,
        store: &EdgeStore,
        route: &RouteKey,
        lease: &GatewayLease,
        scope: &ExecutorScope,
        stream_id: &str,
        now: u64,
    ) -> Result<Option<u64>, HarnessError> {
        let mut transaction = self.transaction(route).await?;
        require_lease(&mut transaction, route, lease, now).await?;
        let cursor = store
            .begin_upload_sync_in_transaction(
                &mut transaction,
                &route.executor_id,
                scope,
                stream_id,
            )
            .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(cursor)
    }

    pub(crate) async fn merge_uploads(
        &self,
        store: &EdgeStore,
        route: &RouteKey,
        lease: &GatewayLease,
        batch: &AcceptedUploadBatch,
        now: u64,
    ) -> Result<Option<u64>, HarnessError> {
        let mut transaction = self.transaction(route).await?;
        require_lease(&mut transaction, route, lease, now).await?;
        let cursor = store
            .merge_uploads_in_transaction(&mut transaction, &route.executor_id, batch)
            .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(cursor)
    }

    pub(crate) async fn merge_events(
        &self,
        store: &EdgeStore,
        route: &RouteKey,
        lease: &GatewayLease,
        session_id: &SessionId,
        events: &[SessionEvent],
        now: u64,
    ) -> Result<Option<u64>, HarnessError> {
        let mut transaction = self.transaction(route).await?;
        require_lease(&mut transaction, route, lease, now).await?;
        store
            .merge_events_in_transaction(
                &mut transaction,
                &route.tenant_id,
                &route.executor_id,
                session_id,
                events,
            )
            .await?;
        let last: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM control_edge_events WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3")
            .bind(route.tenant_id.as_str()).bind(route.executor_id.as_str()).bind(session_id.as_str())
            .fetch_one(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        last.map(unsigned).transpose()
    }

    pub(crate) async fn reply(
        &self,
        route: &RouteKey,
        id: &CommandId,
    ) -> Result<Option<CommandReply>, HarnessError> {
        let mut transaction = self.database.tenant_transaction(&route.tenant_id).await?;
        let reply: Option<String> = sqlx::query_scalar(
            "SELECT reply_json FROM gateway_commands WHERE tenant_id = $1
               AND executor_id = $2 AND command_id = $3 AND state = 'completed'",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        reply.map(|value| decode(&value)).transpose()
    }

    pub(crate) async fn expire(
        &self,
        route: &RouteKey,
        now: u64,
    ) -> Result<Vec<CommandReply>, HarnessError> {
        let mut transaction = self.transaction(route).await?;
        let rows = sqlx::query(
            "SELECT command_id FROM gateway_commands WHERE tenant_id = $1 AND executor_id = $2
               AND state <> 'completed' AND expires_at_ms <= $3 ORDER BY issued_at_ms, command_id",
        )
        .bind(route.tenant_id.as_str())
        .bind(route.executor_id.as_str())
        .bind(integer(now)?)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let mut replies = Vec::with_capacity(rows.len());
        for row in rows {
            let id = CommandId::new(
                row.try_get::<String, _>("command_id")
                    .map_err(database_error)?,
            );
            let reply = CommandReply::failure(
                id,
                now,
                HarnessError::policy("executor command expired before completion"),
            );
            sqlx::query(
                "UPDATE gateway_commands SET state = 'completed', reply_json = $3,
                     completed_at_ms = $4, dispatch_until_ms = NULL
                 WHERE tenant_id = $1 AND command_id = $2",
            )
            .bind(route.tenant_id.as_str())
            .bind(reply.command_id.as_str())
            .bind(encode(&reply)?)
            .bind(integer(now)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            replies.push(reply);
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(replies)
    }
}

async fn require_lease(
    transaction: &mut Transaction<'_, Any>,
    route: &RouteKey,
    lease: &GatewayLease,
    now: u64,
) -> Result<(), HarnessError> {
    let found = sqlx::query(
        "SELECT executor_id FROM gateway_leases WHERE tenant_id = $1 AND executor_id = $2
           AND owner_id = $3 AND fencing_token = $4 AND expires_at_ms > $5",
    )
    .bind(route.tenant_id.as_str())
    .bind(route.executor_id.as_str())
    .bind(&lease.owner_id)
    .bind(integer(lease.fencing_token)?)
    .bind(integer(now)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .is_some();
    if !found {
        return Err(HarnessError::policy(
            "gateway no longer owns the Node lease",
        ));
    }
    Ok(())
}

fn integer(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("gateway integer exceeds storage range"))
}

fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value)
        .map_err(|_| HarnessError::execution("gateway storage contains a negative counter"))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used directly as a map_err adapter."
)]
fn database_error(error: sqlx::Error) -> HarnessError {
    HarnessError::execution(format!("gateway journal: {error}"))
}

fn encode(value: &impl serde::Serialize) -> Result<String, HarnessError> {
    serde_json::to_string(value).map_err(|error| HarnessError::execution(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, HarnessError> {
    serde_json::from_str(value).map_err(|error| HarnessError::execution(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use ternilo_protocol::{SubmissionId, UserId};
    use ternilo_transport::{
        ApplicationOperation, CommandOutcome, ExecutorCommandBody, ExecutorScope,
    };

    #[expect(
        clippy::too_many_lines,
        reason = "Keep the shared persistent delivery and fencing contract in one scenario."
    )]
    async fn delivery_contract(url: &str) {
        let database = Database::connect(url, 4).await.unwrap();
        let journal = GatewayJournal::open(database.clone()).await.unwrap();
        let route = RouteKey::new(
            TenantId::new(format!("journal-owner-{:032x}", rand::random::<u128>())),
            ExecutorId::new("laptop"),
        );
        let other = RouteKey::new(
            TenantId::new(format!("journal-other-{:032x}", rand::random::<u128>())),
            ExecutorId::new("laptop"),
        );
        let command = ExecutorCommand {
            command_id: CommandId::new("journal-command"),
            input_provenance: None,
            scope: ExecutorScope {
                tenant_id: route.tenant_id.clone(),
                user_id: UserId::new("owner"),
            },
            issued_at_ms: 100,
            expires_at_ms: 5_000,
            body: ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionQueueRemove {
                    session_id: SessionId::new("session"),
                    submission_id: SubmissionId::new("submission"),
                },
            },
        };
        journal.enqueue(&route, &command).await.unwrap();
        journal.enqueue(&route, &command).await.unwrap();
        let mut sensitive = command.clone();
        sensitive.command_id = CommandId::new("sensitive-command");
        sensitive.body = ExecutorCommandBody::Application {
            request: ApplicationOperation::CredentialSet {
                name: "API_KEY".to_owned(),
                value: "private-value".to_owned(),
            },
        };
        assert!(journal.enqueue(&route, &sensitive).await.is_err());
        assert!(
            !journal
                .contains(&route, &sensitive.command_id)
                .await
                .unwrap()
        );
        for (index, request) in [
            ApplicationOperation::Snapshot,
            ApplicationOperation::DirectoryList {
                path: Some("/private/host/workspace".to_owned()),
            },
            ApplicationOperation::DirectoryCreate {
                parent: "/private/host/workspace".to_owned(),
                name: "child".to_owned(),
            },
            ApplicationOperation::WorkspaceCreate {
                path: "/private/host/workspace".to_owned(),
            },
            ApplicationOperation::SessionSkills {
                session_id: SessionId::new("session"),
            },
            ApplicationOperation::ExtensionInventory,
        ]
        .into_iter()
        .enumerate()
        {
            let mut private = command.clone();
            private.command_id = CommandId::new(format!("private-command-{index}"));
            private.body = ExecutorCommandBody::Application { request };
            assert!(journal.enqueue(&route, &private).await.is_err());
            assert!(
                !journal.contains(&route, &private.command_id).await.unwrap(),
                "private Node paths must never enter the server journal"
            );
        }
        let mut changed = command.clone();
        changed.expires_at_ms += 1;
        assert!(journal.enqueue(&route, &changed).await.is_err());
        let first = journal
            .acquire(&route, "server-a", 100, 1_000)
            .await
            .unwrap()
            .unwrap();
        assert!(
            journal
                .acquire(&route, "server-b", 101, 1_000)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            journal.claim(&route, &first, 101, 500, 8).await.unwrap(),
            vec![command.clone()]
        );
        assert!(
            journal
                .claim(&route, &first, 102, 500, 8)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(!journal.contains(&other, &command.command_id).await.unwrap());
        drop(journal);
        database.close().await;

        // The next process opens the same journal after an unclean disconnect.
        let database = Database::connect(url, 4).await.unwrap();
        let journal = GatewayJournal::open(database.clone()).await.unwrap();
        let next = journal
            .acquire(&route, "server-b", 1_101, 1_000)
            .await
            .unwrap()
            .unwrap();
        assert!(next.fencing_token > first.fencing_token);
        assert!(!journal.renew(&route, &first, 1_102, 1_000).await.unwrap());
        assert_eq!(
            journal.claim(&route, &next, 1_102, 500, 8).await.unwrap(),
            vec![command.clone()]
        );
        let reply =
            CommandReply::success(command.command_id.clone(), 1_103, json!({"result":"完成"}));
        assert!(
            journal
                .complete(&route, &first, &reply, 1_103)
                .await
                .is_err()
        );
        journal
            .complete(&route, &next, &reply, 1_103)
            .await
            .unwrap();
        journal
            .complete(&route, &next, &reply, 1_104)
            .await
            .unwrap();
        let changed = CommandReply::success(
            command.command_id.clone(),
            1_103,
            json!({"result":"changed"}),
        );
        assert!(
            journal
                .complete(&route, &next, &changed, 1_104)
                .await
                .is_err()
        );
        assert_eq!(
            journal.reply(&route, &command.command_id).await.unwrap(),
            Some(reply)
        );
        assert!(
            journal
                .reply(&other, &command.command_id)
                .await
                .unwrap()
                .is_none()
        );
        journal.release(&route, &next).await.unwrap();
        let released = journal
            .acquire(&route, "server-c", 1_105, 1_000)
            .await
            .unwrap()
            .unwrap();
        assert!(released.fencing_token > next.fencing_token);
        assert!(
            journal
                .claim(&route, &released, 1_105, 500, 8)
                .await
                .unwrap()
                .is_empty()
        );
        let mut expired = command;
        expired.command_id = CommandId::new("expired-command");
        journal.enqueue(&route, &expired).await.unwrap();
        let expired_replies = journal.expire(&route, 5_000).await.unwrap();
        assert_eq!(expired_replies.len(), 1);
        assert!(matches!(
            expired_replies[0].outcome,
            CommandOutcome::Error { .. }
        ));
        assert!(journal.expire(&route, 5_001).await.unwrap().is_empty());
        drop(journal);
        database.close().await;
    }

    #[tokio::test]
    async fn sqlite_delivery_survives_process_restart_and_rejects_stale_owners() {
        let directory = tempfile::tempdir().unwrap();
        delivery_contract(&format!(
            "sqlite:{}",
            directory.path().join("gateway.sqlite3").display()
        ))
        .await;
    }

    #[tokio::test]
    #[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
    async fn postgres_delivery_survives_process_restart_and_rejects_stale_owners() {
        delivery_contract(
            &std::env::var("TERNILO_TEST_DATABASE_URL")
                .expect("disposable PostgreSQL database URL"),
        )
        .await;
    }
}
