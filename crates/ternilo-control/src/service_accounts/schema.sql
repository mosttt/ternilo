CREATE TABLE control_service_accounts (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id),
    service_account_id TEXT NOT NULL REFERENCES control_users(user_id),
    name TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id,service_account_id),
    UNIQUE (service_account_id),
    UNIQUE (tenant_id,name)
);
CREATE TABLE control_service_credentials (
    tenant_id TEXT NOT NULL,
    credential_id TEXT NOT NULL,
    service_account_id TEXT NOT NULL,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    scopes TEXT NOT NULL,
    issued_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > issued_at_ms),
    last_used_at_ms BIGINT,
    revoked_at_ms BIGINT,
    PRIMARY KEY (tenant_id,credential_id),
    FOREIGN KEY (tenant_id,service_account_id)
        REFERENCES control_service_accounts(tenant_id,service_account_id)
);
CREATE INDEX control_service_credentials_account ON control_service_credentials(tenant_id,service_account_id);
