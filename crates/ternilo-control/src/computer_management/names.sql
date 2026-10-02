CREATE TABLE control_computer_names (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    executor_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    name TEXT NOT NULL CHECK (length(name) > 0),
    name_key TEXT,
    reserved_until_ms BIGINT,
    PRIMARY KEY (tenant_id, executor_id),
    UNIQUE (tenant_id, owner_user_id, name_key)
);
