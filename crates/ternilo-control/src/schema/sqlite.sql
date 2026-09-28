-- Final control schema; JSON uses the shared text representation.
CREATE TABLE control_users (
    user_id TEXT PRIMARY KEY,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    email TEXT,
    username TEXT NOT NULL UNIQUE CHECK (username = LOWER(username) AND LENGTH(username) BETWEEN 3 AND 64),
    platform_role TEXT NOT NULL DEFAULT 'user' CHECK (platform_role IN ('user', 'admin', 'operator', 'auditor')),
    role_revision BIGINT NOT NULL DEFAULT 1 CHECK (role_revision > 0),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'pending', 'rejected', 'banned', 'removed')),
    status_revision BIGINT NOT NULL DEFAULT 1 CHECK (status_revision > 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    last_seen_at_ms BIGINT NOT NULL CHECK (last_seen_at_ms >= created_at_ms),
    UNIQUE (issuer, subject)
);

CREATE UNIQUE INDEX control_users_contact_email ON control_users (LOWER(email))
    WHERE email IS NOT NULL AND status <> 'removed';

CREATE INDEX control_users_status_directory ON control_users (status, created_at_ms DESC, user_id DESC);
CREATE INDEX control_users_directory ON control_users (created_at_ms DESC, user_id DESC);
CREATE INDEX control_users_role_directory ON control_users (platform_role, created_at_ms DESC, user_id DESC);

CREATE TABLE control_tenants (
    tenant_id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('personal', 'team')),
    display_name TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0)
);

CREATE UNIQUE INDEX control_personal_space_owner ON control_tenants (created_by) WHERE kind = 'personal';

CREATE TABLE control_memberships (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('viewer', 'member', 'admin', 'owner')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, user_id)
);

CREATE INDEX control_memberships_user ON control_memberships (user_id, tenant_id);

CREATE TABLE control_projects (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, project_id),
    UNIQUE (tenant_id, name)
);

CREATE TABLE control_quotas (
    tenant_id TEXT PRIMARY KEY REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    max_nodes INTEGER NOT NULL CHECK (max_nodes > 0),
    max_concurrent_runs INTEGER NOT NULL CHECK (max_concurrent_runs > 0),
    monthly_model_tokens BIGINT NOT NULL CHECK (monthly_model_tokens > 0),
    max_secrets INTEGER NOT NULL CHECK (max_secrets > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= 0),
    updated_by TEXT NOT NULL REFERENCES control_users(user_id)
);

CREATE TABLE control_quota_usage (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    period_start TEXT NOT NULL,
    used_model_tokens BIGINT NOT NULL DEFAULT 0 CHECK (used_model_tokens >= 0),
    unknown_model_tokens BIGINT NOT NULL DEFAULT 0 CHECK (unknown_model_tokens >= 0),
    PRIMARY KEY (tenant_id, period_start)
);

CREATE TABLE control_quota_reservations (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    reservation_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    run_id TEXT,
    period_start TEXT NOT NULL,
    unknown_model_tokens BIGINT NOT NULL DEFAULT 0 CHECK (unknown_model_tokens >= 0),
    reserved_model_tokens BIGINT NOT NULL CHECK (reserved_model_tokens > 0),
    state TEXT NOT NULL CHECK (state IN ('active', 'committed', 'released', 'expired')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > created_at_ms),
    committed_model_tokens BIGINT CHECK (committed_model_tokens >= 0),
    PRIMARY KEY (tenant_id, reservation_id)
);

CREATE INDEX control_quota_reservations_active
ON control_quota_reservations (tenant_id, state, expires_at_ms);

CREATE TABLE control_executor_enrollments (
    enrollment_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    project_id TEXT,
    executor_id TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms >= 0),
    consumed_at_ms BIGINT,
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    CHECK (project_id IS NULL OR length(project_id) > 0),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id)
);

CREATE TABLE control_executors (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    executor_id TEXT NOT NULL,
    project_id TEXT,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    state TEXT NOT NULL CHECK (state IN ('enrolled', 'active', 'revoked')),
    enrolled_at_ms BIGINT NOT NULL CHECK (enrolled_at_ms >= 0),
    last_seen_at_ms BIGINT,
    PRIMARY KEY (tenant_id, executor_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id)
);

CREATE TABLE control_node_credentials (
    credential_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    issued_at_ms BIGINT NOT NULL CHECK (issued_at_ms >= 0),
    last_used_at_ms BIGINT,
    revoked_at_ms BIGINT,
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE INDEX control_node_credentials_executor
ON control_node_credentials (tenant_id, executor_id);

CREATE TABLE control_workspaces (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 256),
    placement TEXT NOT NULL CHECK (placement IN ('local_node', 'cloud')),
    storage TEXT NOT NULL CHECK (storage IN ('local_path', 'cloud_volume', 'git_worktree')),
    executor_id TEXT,
    executor_workspace_id TEXT,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    unregistered_at_ms BIGINT CHECK (
        unregistered_at_ms IS NULL OR unregistered_at_ms >= created_at_ms
    ),
    PRIMARY KEY (tenant_id, workspace_id),
    UNIQUE (tenant_id, workspace_id, project_id),
    FOREIGN KEY (tenant_id, project_id)
        REFERENCES control_projects(tenant_id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE RESTRICT,
    CHECK (
        (placement = 'cloud'
            AND storage IN ('cloud_volume', 'git_worktree')
            AND executor_id IS NULL
            AND executor_workspace_id IS NULL)
        OR
        (placement = 'local_node'
            AND storage = 'local_path'
            AND executor_id IS NOT NULL
            AND executor_workspace_id IS NOT NULL)
    )
);

CREATE INDEX control_workspaces_owner
ON control_workspaces (tenant_id, owner_user_id, updated_at_ms DESC);

CREATE UNIQUE INDEX control_workspaces_registered_name
ON control_workspaces (tenant_id, owner_user_id, project_id, name)
WHERE unregistered_at_ms IS NULL;

CREATE UNIQUE INDEX control_workspaces_executor_binding
ON control_workspaces (tenant_id, executor_id, executor_workspace_id)
WHERE placement = 'local_node';

CREATE TABLE control_edge_executors (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    hello_json TEXT NOT NULL,
    last_seen_at_ms BIGINT NOT NULL CHECK (last_seen_at_ms >= 0),
    PRIMARY KEY (tenant_id, executor_id),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE TABLE control_edge_session_provenance (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    parent_session_id TEXT,
    subagent_id TEXT,
    PRIMARY KEY (tenant_id, executor_id, session_id),
    UNIQUE (tenant_id, executor_id, parent_session_id, subagent_id),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE,
    CHECK (subagent_id IS NULL OR parent_session_id IS NOT NULL),
    CHECK (parent_session_id IS NULL OR parent_session_id <> session_id)
);

CREATE TABLE control_edge_input_provenance (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    input_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    target_subagent_id TEXT,
    provenance_json TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, executor_id, input_id),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_edge_executors(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE TABLE control_edge_upload_streams (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    stream_id TEXT NOT NULL,
    last_seq BIGINT NOT NULL DEFAULT 0 CHECK (last_seq >= 0),
    PRIMARY KEY (tenant_id, executor_id),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE TABLE control_edge_deleted_sessions (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id, session_id),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_edge_upload_streams(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE TABLE control_edge_session_uploads (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    attachment_index BIGINT NOT NULL CHECK (attachment_index >= 0 AND attachment_index < 10),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    submitted_run_id TEXT NOT NULL,
    name TEXT NOT NULL,
    media_type TEXT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id, session_id, submission_id, attachment_index),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_edge_upload_streams(tenant_id, executor_id) ON DELETE CASCADE
);
CREATE INDEX control_edge_session_uploads_order
ON control_edge_session_uploads (tenant_id, created_at_ms DESC, session_id, submission_id, attachment_index);

CREATE TABLE control_edge_events (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    event_json TEXT NOT NULL,
    PRIMARY KEY (tenant_id, executor_id, session_id, seq),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE TABLE control_edge_sessions (
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES control_users(user_id),
    node_session_id TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    last_event_seq BIGINT CHECK (last_event_seq IS NULL OR last_event_seq >= 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, session_id),
    UNIQUE (tenant_id, executor_id, node_session_id),
    FOREIGN KEY (tenant_id, workspace_id)
        REFERENCES control_workspaces(tenant_id, workspace_id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE INDEX control_edge_sessions_owner
ON control_edge_sessions (tenant_id, owner_user_id, updated_at_ms DESC);

CREATE TABLE control_secrets (
    secret_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    project_id TEXT,
    name TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    nonce BLOB NOT NULL CHECK (length(nonce) = 24),
    ciphertext BLOB NOT NULL,
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    deleted_at_ms BIGINT,
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id)
);

CREATE TABLE control_secret_heads (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    project_id TEXT,
    name TEXT NOT NULL,
    secret_id TEXT NOT NULL UNIQUE REFERENCES control_secrets(secret_id),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id)
);

CREATE TABLE control_audit_log (
    audit_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    audit_id TEXT NOT NULL UNIQUE,
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE RESTRICT,
    actor_user_id TEXT REFERENCES control_users(user_id),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('user', 'node', 'worker', 'system')),
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'denied', 'failure')),
    metadata TEXT NOT NULL,
    previous_hash BLOB CHECK (previous_hash IS NULL OR length(previous_hash) = 32),
    entry_hash BLOB NOT NULL CHECK (length(entry_hash) = 32),
    occurred_at_ms BIGINT NOT NULL CHECK (occurred_at_ms >= 0)
);

CREATE INDEX control_audit_log_tenant_sequence
ON control_audit_log (tenant_id, audit_sequence);


CREATE TABLE control_extension_publishers (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    key_id TEXT NOT NULL,
    trust TEXT NOT NULL,
    revoked BIGINT NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1)),
    added_at_ms BIGINT NOT NULL CHECK (added_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= added_at_ms),
    added_by TEXT NOT NULL REFERENCES control_users(user_id),
    PRIMARY KEY (tenant_id, key_id)
);

CREATE TABLE control_extension_packages (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    package_id TEXT NOT NULL,
    version TEXT NOT NULL,
    publisher_key_id TEXT NOT NULL,
    install_request TEXT NOT NULL,
    enabled BIGINT NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    revoked BIGINT NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1)),
    installed_at_ms BIGINT NOT NULL CHECK (installed_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= installed_at_ms),
    installed_by TEXT NOT NULL REFERENCES control_users(user_id),
    PRIMARY KEY (tenant_id, package_id, version),
    FOREIGN KEY (tenant_id, publisher_key_id)
        REFERENCES control_extension_publishers(tenant_id, key_id)
);

CREATE INDEX control_extension_packages_publisher
ON control_extension_packages (tenant_id, publisher_key_id);


CREATE TABLE control_user_agent_presets (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    preset_id TEXT NOT NULL,
    document_json TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, preset_id),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);

CREATE TABLE control_user_preferences (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    default_agent_preset TEXT NOT NULL,
    default_model TEXT,
    sidebar_ordering TEXT NOT NULL DEFAULT '{"workspace_order":[],"session_order_by_account":{}}',
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (tenant_id, user_id),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);

CREATE TABLE control_user_credentials (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    nonce BLOB NOT NULL CHECK (length(nonce) = 24),
    ciphertext BLOB NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, name),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);

CREATE TABLE control_user_credential_records (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    record_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    nonce BLOB NOT NULL CHECK (length(nonce) = 24),
    ciphertext BLOB NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, record_key),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);


CREATE TABLE control_user_provider_profiles (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_json TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, provider_id),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);


CREATE UNIQUE INDEX control_secrets_scope_version ON control_secrets (tenant_id, (COALESCE(project_id, '')), name, version);
CREATE UNIQUE INDEX control_secret_heads_scope_name ON control_secret_heads (tenant_id, (COALESCE(project_id, '')), name);

CREATE TRIGGER control_audit_no_update BEFORE UPDATE ON control_audit_log
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER control_audit_no_delete BEFORE DELETE ON control_audit_log
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
