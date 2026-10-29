CREATE TABLE control_client_maintenance (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    credential_id TEXT NOT NULL,
    instance_nonce TEXT NOT NULL,
    hello_json TEXT NOT NULL,
    last_seen_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id),
    FOREIGN KEY (tenant_id, executor_id) REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);
CREATE TABLE control_maintenance_commands (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    operation_json TEXT NOT NULL,
    reply_json TEXT,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id, command_id),
    FOREIGN KEY (tenant_id, executor_id) REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);
