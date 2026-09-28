CREATE TABLE control_users (
    user_id TEXT PRIMARY KEY,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    email TEXT,
    display_name TEXT,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    last_seen_at_ms BIGINT NOT NULL CHECK (last_seen_at_ms >= created_at_ms),
    UNIQUE (issuer, subject)
);

CREATE TABLE control_tenants (
    tenant_id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0)
);

CREATE TABLE control_memberships (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('viewer', 'member', 'admin', 'owner')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (tenant_id, user_id)
);

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
    period_start DATE NOT NULL,
    used_model_tokens BIGINT NOT NULL DEFAULT 0 CHECK (used_model_tokens >= 0),
    PRIMARY KEY (tenant_id, period_start)
);

CREATE TABLE control_quota_reservations (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    reservation_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    run_id TEXT,
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
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
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
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
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
    hello_json JSONB NOT NULL,
    last_seen_at_ms BIGINT NOT NULL CHECK (last_seen_at_ms >= 0),
    PRIMARY KEY (tenant_id, executor_id),
    FOREIGN KEY (tenant_id, executor_id)
        REFERENCES control_executors(tenant_id, executor_id) ON DELETE CASCADE
);

CREATE TABLE control_edge_events (
    tenant_id TEXT NOT NULL,
    executor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq BIGINT NOT NULL CHECK (seq >= 0),
    event_json JSONB NOT NULL,
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
    metadata_json JSONB NOT NULL,
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
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 24),
    ciphertext BYTEA NOT NULL,
    created_by TEXT NOT NULL REFERENCES control_users(user_id),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    deleted_at_ms BIGINT,
    CONSTRAINT control_secrets_scope_version
        UNIQUE NULLS NOT DISTINCT (tenant_id, project_id, name, version),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id)
);

CREATE TABLE control_secret_heads (
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE CASCADE,
    project_id TEXT,
    name TEXT NOT NULL,
    secret_id TEXT NOT NULL UNIQUE REFERENCES control_secrets(secret_id),
    CONSTRAINT control_secret_heads_scope_name
        UNIQUE NULLS NOT DISTINCT (tenant_id, project_id, name),
    FOREIGN KEY (tenant_id, project_id) REFERENCES control_projects(tenant_id, project_id)
);

CREATE TABLE control_audit_log (
    audit_sequence BIGSERIAL PRIMARY KEY,
    audit_id TEXT NOT NULL UNIQUE,
    tenant_id TEXT NOT NULL REFERENCES control_tenants(tenant_id) ON DELETE RESTRICT,
    actor_user_id TEXT REFERENCES control_users(user_id),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('user', 'node', 'worker', 'system')),
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'denied', 'failure')),
    metadata JSONB NOT NULL,
    previous_hash BYTEA CHECK (previous_hash IS NULL OR octet_length(previous_hash) = 32),
    entry_hash BYTEA NOT NULL CHECK (octet_length(entry_hash) = 32),
    occurred_at_ms BIGINT NOT NULL CHECK (occurred_at_ms >= 0)
);

CREATE INDEX control_audit_log_tenant_sequence
ON control_audit_log (tenant_id, audit_sequence);

CREATE FUNCTION ternilo_reject_audit_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'control_audit_log is append-only';
END;
$$;

CREATE TRIGGER control_audit_log_immutable
BEFORE UPDATE OR DELETE ON control_audit_log
FOR EACH ROW EXECUTE FUNCTION ternilo_reject_audit_mutation();

CREATE FUNCTION ternilo_consume_enrollment(
    p_token_hash BYTEA,
    p_now_ms BIGINT,
    p_credential_id TEXT,
    p_credential_hash BYTEA
) RETURNS TABLE (tenant_id TEXT, user_id TEXT, project_id TEXT, executor_id TEXT, credential_id TEXT)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    enrollment control_executor_enrollments%ROWTYPE;
BEGIN
    SELECT * INTO enrollment
    FROM control_executor_enrollments
    WHERE token_hash = p_token_hash
    FOR UPDATE;

    IF NOT FOUND OR enrollment.consumed_at_ms IS NOT NULL OR enrollment.expires_at_ms <= p_now_ms THEN
        RETURN;
    END IF;

    UPDATE control_executor_enrollments
    SET consumed_at_ms = p_now_ms
    WHERE enrollment_id = enrollment.enrollment_id;

    INSERT INTO control_executors
        (tenant_id, executor_id, project_id, owner_user_id, state, enrolled_at_ms)
    VALUES
        (enrollment.tenant_id, enrollment.executor_id, enrollment.project_id,
         enrollment.created_by, 'enrolled', p_now_ms)
    ON CONFLICT ON CONSTRAINT control_executors_pkey DO UPDATE
    SET project_id = EXCLUDED.project_id, owner_user_id = EXCLUDED.owner_user_id,
        state = 'enrolled', enrolled_at_ms = EXCLUDED.enrolled_at_ms;

    INSERT INTO control_node_credentials
        (credential_id, tenant_id, executor_id, token_hash, issued_at_ms)
    VALUES
        (p_credential_id, enrollment.tenant_id, enrollment.executor_id,
         p_credential_hash, p_now_ms);

    RETURN QUERY SELECT enrollment.tenant_id, enrollment.created_by, enrollment.project_id,
                        enrollment.executor_id, p_credential_id;
END;
$$;

CREATE FUNCTION ternilo_authenticate_node(p_token_hash BYTEA, p_now_ms BIGINT)
RETURNS TABLE (tenant_id TEXT, user_id TEXT, project_id TEXT, executor_id TEXT, credential_id TEXT)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
BEGIN
    RETURN QUERY
    UPDATE control_node_credentials AS credential
    SET last_used_at_ms = p_now_ms
    FROM control_executors AS executor
    WHERE credential.token_hash = p_token_hash
      AND credential.revoked_at_ms IS NULL
      AND executor.tenant_id = credential.tenant_id
      AND executor.executor_id = credential.executor_id
      AND executor.state != 'revoked'
    RETURNING credential.tenant_id, executor.owner_user_id, executor.project_id,
              credential.executor_id, credential.credential_id;
END;
$$;

CREATE FUNCTION ternilo_list_user_tenants(p_user_id TEXT)
RETURNS TABLE (tenant_id TEXT, slug TEXT, display_name TEXT, role TEXT)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT tenant.tenant_id, tenant.slug, tenant.display_name, membership.role
    FROM control_memberships AS membership
    JOIN control_tenants AS tenant USING (tenant_id)
    WHERE membership.user_id = p_user_id
    ORDER BY tenant.created_at_ms, tenant.tenant_id
$$;

REVOKE ALL ON FUNCTION ternilo_consume_enrollment(
    BYTEA, BIGINT, TEXT, BYTEA
) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_authenticate_node(BYTEA, BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_list_user_tenants(TEXT) FROM PUBLIC;

-- Docker creates the runtime role before migrations. Library users may choose a
-- different role and grant these three entry points themselves.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        GRANT EXECUTE ON FUNCTION ternilo_consume_enrollment(
            BYTEA, BIGINT, TEXT, BYTEA
        ) TO ternilo_runtime;
        GRANT EXECUTE ON FUNCTION ternilo_authenticate_node(BYTEA, BIGINT)
            TO ternilo_runtime;
        GRANT EXECUTE ON FUNCTION ternilo_list_user_tenants(TEXT)
            TO ternilo_runtime;
    END IF;
END;
$$;

ALTER TABLE control_tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_projects ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_quotas ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_quota_usage ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_quota_reservations ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_executor_enrollments ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_executors ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_node_credentials ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_workspaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_edge_executors ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_edge_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_edge_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_secret_heads ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_audit_log ENABLE ROW LEVEL SECURITY;

CREATE POLICY control_tenants_scope ON control_tenants
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_memberships_scope ON control_memberships
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_projects_scope ON control_projects
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_quotas_scope ON control_quotas
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_quota_usage_scope ON control_quota_usage
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_quota_reservations_scope ON control_quota_reservations
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_executor_enrollments_scope ON control_executor_enrollments
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_executors_scope ON control_executors
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_node_credentials_scope ON control_node_credentials
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_workspaces_scope ON control_workspaces
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_edge_executors_scope ON control_edge_executors
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_edge_events_scope ON control_edge_events
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_edge_sessions_scope ON control_edge_sessions
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_secrets_scope ON control_secrets
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_secret_heads_scope ON control_secret_heads
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_audit_log_scope ON control_audit_log
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
