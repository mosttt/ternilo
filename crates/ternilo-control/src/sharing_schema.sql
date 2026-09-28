CREATE TABLE control_resource_shares (
    tenant_id TEXT NOT NULL,
    resource_kind TEXT NOT NULL CHECK (resource_kind IN ('workspace', 'session')),
    resource_id TEXT NOT NULL,
    grantee_user_id TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    granted_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, resource_kind, resource_id, grantee_user_id),
    FOREIGN KEY (tenant_id, grantee_user_id) REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);

CREATE TABLE control_permission_groups (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    group_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, group_id),
    UNIQUE (tenant_id, name)
);

CREATE TABLE control_permission_group_members (
    tenant_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, group_id, user_id),
    FOREIGN KEY (tenant_id, group_id) REFERENCES control_permission_groups(tenant_id, group_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, user_id) REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);
CREATE INDEX control_permission_groups_by_user ON control_permission_group_members(tenant_id, user_id, group_id);

CREATE TABLE control_resource_group_shares (
    tenant_id TEXT NOT NULL,
    resource_kind TEXT NOT NULL CHECK (resource_kind IN ('workspace', 'session')),
    resource_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    granted_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, resource_kind, resource_id, group_id),
    FOREIGN KEY (tenant_id, group_id) REFERENCES control_permission_groups(tenant_id, group_id) ON DELETE CASCADE
);

CREATE TABLE control_resource_fork_group_sources (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    source_resource_kind TEXT NOT NULL,
    source_resource_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, session_id, user_id, source_resource_kind, source_resource_id, group_id),
    FOREIGN KEY (tenant_id, source_resource_kind, source_resource_id, group_id)
        REFERENCES control_resource_group_shares(tenant_id, resource_kind, resource_id, group_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, group_id, user_id)
        REFERENCES control_permission_group_members(tenant_id, group_id, user_id) ON DELETE CASCADE
);
