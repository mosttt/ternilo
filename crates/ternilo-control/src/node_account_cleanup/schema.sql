CREATE TABLE control_node_storage_bindings (
    credential_id TEXT PRIMARY KEY REFERENCES control_node_credentials(credential_id),
    tenant_id TEXT NOT NULL,
    storage_instance_id TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL
);
CREATE TABLE control_node_input_authorizations (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    input_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    status_revision BIGINT NOT NULL CHECK (status_revision >= 0),
    credential_id TEXT NOT NULL REFERENCES control_node_credentials(credential_id),
    PRIMARY KEY (tenant_id, executor_id, input_id),
    FOREIGN KEY (tenant_id, executor_id, input_id)
      REFERENCES control_edge_input_provenance(tenant_id, executor_id, input_id)
);
CREATE INDEX control_node_input_authorizations_actor
ON control_node_input_authorizations (user_id, credential_id);
CREATE TABLE control_node_account_cleanup (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    credential_id TEXT NOT NULL REFERENCES control_node_credentials(credential_id),
    request_id TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    status_revision BIGINT NOT NULL CHECK (status_revision >= 0),
    created_at_ms BIGINT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','confirmed')),
    detail TEXT,
    confirmed_at_ms BIGINT,
    PRIMARY KEY (credential_id, user_id, status_revision),
    FOREIGN KEY (tenant_id, executor_id) REFERENCES control_executors(tenant_id, executor_id)
);
CREATE INDEX control_node_account_cleanup_actor
ON control_node_account_cleanup (user_id, tenant_id, executor_id);
