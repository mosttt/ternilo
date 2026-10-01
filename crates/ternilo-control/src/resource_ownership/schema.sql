CREATE TABLE control_resource_ownership (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    resource_kind TEXT NOT NULL CHECK (resource_kind IN ('workspace', 'session')),
    resource_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    revision BIGINT NOT NULL CHECK (revision > 0),
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, resource_kind, resource_id)
);
CREATE INDEX control_resource_ownership_by_owner
ON control_resource_ownership(tenant_id, owner_user_id, resource_kind, resource_id);
