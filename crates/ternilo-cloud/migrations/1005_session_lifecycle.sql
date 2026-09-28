ALTER TABLE cloud_sessions
    ADD COLUMN parent_session_id TEXT,
    ADD COLUMN archived_at_ms BIGINT CHECK (archived_at_ms >= created_at_ms),
    ADD CONSTRAINT cloud_sessions_parent
        FOREIGN KEY (tenant_id, parent_session_id)
        REFERENCES cloud_sessions(tenant_id, session_id);

CREATE INDEX cloud_sessions_active_owner
ON cloud_sessions (tenant_id, user_id, updated_at_ms DESC)
WHERE archived_at_ms IS NULL;
