use std::net::IpAddr;

use ternilo_protocol::HarnessError;
use ternilo_storage::{Database, database_error};

use crate::{
    ControlStore, ControlUser,
    crypto::{hex, token_hash},
};

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database.initialize("browser_session_details", 1, r"
        CREATE TABLE control_browser_session_details (
            token_hash TEXT PRIMARY KEY REFERENCES control_browser_sessions(token_hash) ON DELETE CASCADE,
            user_agent TEXT,
            first_ip TEXT,
            last_ip TEXT,
            last_active_at_ms BIGINT NOT NULL
        );
    ", r"
        REVOKE ALL ON control_browser_session_details FROM PUBLIC;
        DO $$ BEGIN
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
            GRANT SELECT, INSERT, UPDATE, DELETE ON control_browser_session_details TO ternilo_runtime;
        END IF;
        END $$;
    ").await
}

impl ControlStore {
    /// Record authenticated HTTP activity without trusting client-supplied identity fields.
    pub async fn record_browser_session_activity(
        &self,
        actor: &ControlUser,
        token: &str,
        user_agent: Option<&str>,
        ip: Option<IpAddr>,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        if !token.starts_with("kns_") {
            return Ok(());
        }
        let now =
            i64::try_from(now_ms).map_err(|_| HarnessError::invalid("timestamp exceeds i64"))?;
        let agent = user_agent.map(|value| value.chars().take(512).collect::<String>());
        let ip = ip.map(|value| value.to_string());
        sqlx::query(r"
            INSERT INTO control_browser_session_details
                (token_hash, user_agent, first_ip, last_ip, last_active_at_ms)
            SELECT token_hash, $3, $4, $4, $5 FROM control_browser_sessions
            WHERE token_hash = $1 AND user_id = $2 AND revoked_at_ms IS NULL AND expires_at_ms > $5
            ON CONFLICT(token_hash) DO UPDATE SET
                last_ip = excluded.last_ip,
                last_active_at_ms = excluded.last_active_at_ms
            WHERE control_browser_session_details.last_active_at_ms <= $6
               OR COALESCE(control_browser_session_details.last_ip, '') <> COALESCE(excluded.last_ip, '')
        ")
        .bind(hex(&token_hash(token))).bind(actor.user_id.as_str()).bind(agent)
        .bind(ip).bind(now).bind(now.saturating_sub(60_000))
        .execute(&self.pool).await.map_err(database_error)?;
        Ok(())
    }
}
