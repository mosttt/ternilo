CREATE TABLE control_computer_model_requests (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id),
    request_id TEXT NOT NULL,
    credential_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    execution_executor_id TEXT NOT NULL,
    source_executor_id TEXT NOT NULL,
    actor_user_id TEXT NOT NULL,
    model_owner_user_id TEXT NOT NULL,
    resource_owner_user_id TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    max_attempts BIGINT NOT NULL CHECK (max_attempts BETWEEN 1 AND 8),
    state TEXT NOT NULL CHECK (state IN ('pending','completed','failed','cancelled')),
    error_code TEXT,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id,request_id),
    UNIQUE (tenant_id,credential_id,session_id,run_id,request_key)
);
CREATE INDEX control_computer_model_requests_owner ON control_computer_model_requests(tenant_id,model_owner_user_id,created_at_ms,request_id);
CREATE INDEX control_computer_model_requests_actor ON control_computer_model_requests(tenant_id,actor_user_id,created_at_ms,request_id);
CREATE TABLE control_computer_model_attempts (
    tenant_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    attempt BIGINT NOT NULL CHECK (attempt BETWEEN 1 AND 8),
    report_json TEXT,
    started_at_ms BIGINT NOT NULL,
    finished_at_ms BIGINT,
    PRIMARY KEY (tenant_id,request_id,attempt),
    FOREIGN KEY (tenant_id,request_id) REFERENCES control_computer_model_requests(tenant_id,request_id)
);
