CREATE TABLE cloud_session_stats (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    checkpoint_json TEXT NOT NULL,
    PRIMARY KEY (tenant_id, session_id),
    FOREIGN KEY (tenant_id, session_id)
        REFERENCES cloud_sessions(tenant_id, session_id) ON DELETE CASCADE
);
