CREATE TABLE control_computer_management (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    display_name TEXT,
    notes TEXT NOT NULL DEFAULT '',
    suspended_at_ms BIGINT,
    removed_at_ms BIGINT,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    PRIMARY KEY (tenant_id, executor_id),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);
