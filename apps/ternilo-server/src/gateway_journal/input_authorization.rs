use super::{
    Any, CommandId, CommandReply, Database, ExecutorCommand, HarnessError, RouteKey, Row,
    Transaction, database_error, encode, integer, lock,
};
use ternilo_protocol::InputAuthor;

const SCHEMA: &str = r"
CREATE TABLE gateway_input_authorizations (
    tenant_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    status_revision BIGINT NOT NULL,
    invalidated BIGINT NOT NULL DEFAULT 0 CHECK (invalidated IN (0, 1)),
    PRIMARY KEY (tenant_id, command_id),
    FOREIGN KEY (tenant_id, command_id) REFERENCES gateway_commands(tenant_id, command_id) ON DELETE CASCADE
);
";
const POSTGRES: &str = r"
ALTER TABLE gateway_input_authorizations ENABLE ROW LEVEL SECURITY;
CREATE POLICY gateway_input_authorization_scope ON gateway_input_authorizations
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_input_authorizations TO ternilo_runtime;
END IF;
END $$;
";

pub(super) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize("gateway_input_authorization", 1, SCHEMA, POSTGRES)
        .await
}

pub(super) async fn accept(
    tx: &mut Transaction<'static, Any>,
    route: &RouteKey,
    command: &ExecutorCommand,
    inserted: bool,
) -> Result<(), HarnessError> {
    let Some(provenance) = &command.input_provenance else {
        return Ok(());
    };
    let InputAuthor::Account { user_id, .. } = &provenance.author else {
        return Err(HarnessError::policy(
            "remote input requires an authenticated account",
        ));
    };
    // Serialize acceptance with ban/removal; never refresh an old command's authorization.
    lock(tx, &format!("ternilo:account-role:{user_id}")).await?;
    let row = sqlx::query("SELECT status, status_revision FROM control_users WHERE user_id = $1")
        .bind(user_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("input account no longer exists"))?;
    let status: String = row.try_get("status").map_err(database_error)?;
    if status != "active" {
        return Err(HarnessError::policy("input account is no longer active"));
    }
    if inserted {
        let revision: i64 = row.try_get("status_revision").map_err(database_error)?;
        sqlx::query("INSERT INTO gateway_input_authorizations (tenant_id, command_id, user_id, status_revision) VALUES ($1, $2, $3, $4)")
            .bind(route.tenant_id.as_str()).bind(command.command_id.as_str()).bind(user_id.as_str()).bind(revision)
            .execute(&mut **tx).await.map_err(database_error)?;
    }
    Ok(())
}

pub(super) async fn may_dispatch(
    tx: &mut Transaction<'static, Any>,
    route: &RouteKey,
    command: &ExecutorCommand,
    state: &str,
    now: u64,
) -> Result<bool, HarnessError> {
    let Some(provenance) = &command.input_provenance else {
        return Ok(true);
    };
    let InputAuthor::Account { user_id, .. } = &provenance.author else {
        return Err(HarnessError::policy(
            "stored remote input has no authenticated account",
        ));
    };
    let authorized: Option<String> = sqlx::query_scalar(
        "SELECT auth.user_id FROM gateway_input_authorizations auth
         JOIN control_users account ON account.user_id = auth.user_id
         WHERE auth.tenant_id = $1 AND auth.command_id = $2 AND auth.user_id = $3
           AND auth.invalidated = 0 AND account.status = 'active'
           AND account.status_revision = auth.status_revision",
    )
    .bind(route.tenant_id.as_str())
    .bind(command.command_id.as_str())
    .bind(user_id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    if authorized.is_some() {
        return Ok(true);
    }
    // Missing admission evidence from older versions cannot establish safe replay.
    sqlx::query("INSERT INTO gateway_input_authorizations (tenant_id, command_id, user_id, status_revision, invalidated)
                 VALUES ($1, $2, $3, -1, 1) ON CONFLICT (tenant_id, command_id) DO UPDATE SET invalidated = 1")
        .bind(route.tenant_id.as_str()).bind(command.command_id.as_str()).bind(user_id.as_str())
        .execute(&mut **tx).await.map_err(database_error)?;
    if state == "pending" {
        let reply = CommandReply::failure(
            command.command_id.clone(),
            now,
            HarnessError::policy(
                "input delivery authorization is no longer valid; prior execution is not confirmed",
            ),
        );
        sqlx::query(
            "UPDATE gateway_commands SET state = 'completed', reply_json = $3, completed_at_ms = $4
                     WHERE tenant_id = $1 AND command_id = $2",
        )
        .bind(route.tenant_id.as_str())
        .bind(command.command_id.as_str())
        .bind(encode(&reply)?)
        .bind(integer(now)?)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    // An inflight command may already have executed. Keep accepting its actual reply;
    // withholding redelivery does not prove cancellation or undo its effects.
    Ok(false)
}

// The current fenced connection may return a real receipt without replaying a revoked input.
pub(super) async fn is_withheld(
    tx: &mut Transaction<'static, Any>,
    route: &RouteKey,
    command_id: &CommandId,
) -> Result<bool, HarnessError> {
    let found: Option<String> = sqlx::query_scalar("SELECT command_id FROM gateway_input_authorizations WHERE tenant_id = $1 AND command_id = $2 AND invalidated = 1")
        .bind(route.tenant_id.as_str()).bind(command_id.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
    Ok(found.is_some())
}
