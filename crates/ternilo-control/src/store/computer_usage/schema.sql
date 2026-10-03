CREATE TABLE control_edge_usage_events (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq BIGINT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('provider_usage_started','provider_usage_finished')),
    run_id TEXT NOT NULL,
    occurred_at_ms BIGINT NOT NULL,
    started_seq BIGINT,
    source_session_id TEXT,
    provider TEXT,
    model TEXT,
    protocol TEXT,
    event_json TEXT NOT NULL,
    PRIMARY KEY(tenant_id,executor_id,session_id,seq),
    FOREIGN KEY(tenant_id,executor_id,session_id,seq) REFERENCES control_edge_events(tenant_id,executor_id,session_id,seq) ON DELETE CASCADE
);
DROP INDEX IF EXISTS control_edge_usage_start;
DROP INDEX IF EXISTS control_edge_usage_finish;
CREATE INDEX control_edge_usage_start ON control_edge_usage_events(tenant_id,executor_id,kind,occurred_at_ms,session_id,seq);
CREATE INDEX control_edge_usage_finish ON control_edge_usage_events(tenant_id,executor_id,session_id,kind,started_seq,run_id,seq);
