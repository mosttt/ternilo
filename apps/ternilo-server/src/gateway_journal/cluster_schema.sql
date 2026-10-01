CREATE TABLE gateway_peer_routes (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL,
    endpoint TEXT NOT NULL,
    principal_json TEXT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id)
);
CREATE TABLE gateway_live_routes (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    expires_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id)
);
