CREATE TABLE control_instance_settings (
    singleton BIGINT PRIMARY KEY CHECK (singleton = 1),
    mode TEXT NOT NULL CHECK (mode IN ('single_user', 'multi_user')),
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    revision BIGINT NOT NULL CHECK (revision > 0),
    registration_mode TEXT NOT NULL DEFAULT 'invite' CHECK (registration_mode IN ('open', 'invite')),
    registration_require_approval BIGINT NOT NULL DEFAULT 0 CHECK (registration_require_approval IN (0, 1)),
    registration_revision BIGINT NOT NULL DEFAULT 1 CHECK (registration_revision > 0),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    CHECK (registration_mode = 'open' OR registration_require_approval = 0)
);

CREATE TABLE control_account_spaces (
    user_id TEXT PRIMARY KEY REFERENCES control_users(user_id) ON DELETE CASCADE,
    personal_tenant_id TEXT NOT NULL UNIQUE REFERENCES control_tenants(tenant_id),
    default_project_id TEXT NOT NULL,
    FOREIGN KEY (personal_tenant_id, default_project_id)
        REFERENCES control_projects(tenant_id, project_id)
);

CREATE TABLE control_platform_audit (
    sequence BIGINT PRIMARY KEY,
    audit_id TEXT NOT NULL UNIQUE,
    actor_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    action TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    metadata TEXT NOT NULL,
    occurred_at_ms BIGINT NOT NULL,
    previous_hash TEXT,
    entry_hash TEXT NOT NULL
);

CREATE TABLE control_native_accounts (
    user_id TEXT PRIMARY KEY REFERENCES control_users(user_id) ON DELETE CASCADE,
    password_hash TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL
);

CREATE TABLE control_browser_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES control_users(user_id) ON DELETE CASCADE,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > created_at_ms),
    revoked_at_ms BIGINT
);

CREATE INDEX control_browser_sessions_user ON control_browser_sessions(user_id);

CREATE TABLE control_user_invitations (
    invitation_id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    tenant_id TEXT REFERENCES control_tenants(tenant_id),
    role TEXT NOT NULL CHECK (role IN ('viewer', 'member', 'admin')),
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > created_at_ms),
    consumed_at_ms BIGINT,
    consumed_by TEXT REFERENCES control_users(user_id)
);
