use std::net::IpAddr;

use ternilo_protocol::HarnessError;
use ternilo_storage::{Database, database_error};

use crate::{
    ControlStore, ControlUser,
    crypto::{hex, token_hash},
};

pub(super) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database.initialize("oidc_session_details", 1, r"
        CREATE TABLE control_oidc_session_details (
            session_id TEXT PRIMARY KEY REFERENCES control_oidc_sessions(session_id) ON DELETE CASCADE,
            created_at_ms BIGINT,
            user_agent TEXT,
            first_ip TEXT,
            last_ip TEXT,
            last_active_at_ms BIGINT
        );
    ", r"
        REVOKE ALL ON control_oidc_session_details FROM PUBLIC;
        DO $$ BEGIN
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
            GRANT SELECT, INSERT, UPDATE, DELETE ON control_oidc_session_details TO ternilo_runtime;
        END IF;
        END $$;
    ").await
}

impl ControlStore {
    pub(crate) async fn record_oidc_session_activity(
        &self,
        actor: &ControlUser,
        token: &str,
        user_agent: Option<&str>,
        ip: Option<IpAddr>,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let now = super::timestamp(now_ms)?;
        let agent = user_agent.map(|value| value.chars().take(512).collect::<String>());
        let ip = ip.map(|value| value.to_string());
        sqlx::query(r"
            INSERT INTO control_oidc_session_details
                (session_id, user_agent, first_ip, last_ip, last_active_at_ms)
            SELECT s.session_id, $3, $4, $4, $5 FROM control_oidc_sessions s
            JOIN control_users u ON u.issuer=s.issuer AND u.subject=s.subject
            WHERE s.token_hash=$1 AND u.user_id=$2 AND s.access_expires_at_ms>$5 AND s.expires_at_ms>$5
            ON CONFLICT(session_id) DO UPDATE SET
                user_agent=COALESCE(control_oidc_session_details.user_agent,excluded.user_agent),
                first_ip=COALESCE(control_oidc_session_details.first_ip,excluded.first_ip),
                last_ip=excluded.last_ip,
                last_active_at_ms=excluded.last_active_at_ms
            WHERE control_oidc_session_details.last_active_at_ms IS NULL
               OR control_oidc_session_details.last_active_at_ms <= $6
               OR COALESCE(control_oidc_session_details.last_ip,'') <> COALESCE(excluded.last_ip,'')
        ")
        .bind(hex(&token_hash(token))).bind(actor.user_id.as_str()).bind(agent)
        .bind(ip).bind(now).bind(now.saturating_sub(60_000))
        .execute(&self.pool).await.map_err(database_error)?;
        Ok(())
    }
}
