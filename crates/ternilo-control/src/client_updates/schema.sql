CREATE TABLE control_client_updates (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    job_json TEXT NOT NULL,
    download_token_hash TEXT NOT NULL,
    download_expires_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id),
    UNIQUE (tenant_id, operation_id),
    FOREIGN KEY (tenant_id, executor_id) REFERENCES control_executors(tenant_id, executor_id)
);
