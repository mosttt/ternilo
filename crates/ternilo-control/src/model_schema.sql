CREATE TABLE control_model_providers (
    provider_id TEXT PRIMARY KEY,
    profile_json TEXT NOT NULL,
    enabled BIGINT NOT NULL CHECK (enabled IN (0, 1)),
    credential_version BIGINT NOT NULL DEFAULT 0,
    credential_nonce TEXT,
    credential_ciphertext TEXT,
    key_rotation_id TEXT,
    key_rotation_started_at_ms BIGINT,
    previous_credential_version BIGINT,
    previous_credential_nonce TEXT,
    previous_credential_ciphertext TEXT,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    CHECK ((credential_nonce IS NULL) = (credential_ciphertext IS NULL)),
    CHECK ((previous_credential_nonce IS NULL) = (previous_credential_ciphertext IS NULL)),
    CHECK ((key_rotation_id IS NULL AND key_rotation_started_at_ms IS NULL AND previous_credential_version IS NULL
            AND previous_credential_nonce IS NULL AND previous_credential_ciphertext IS NULL)
        OR (key_rotation_id IS NOT NULL AND key_rotation_started_at_ms IS NOT NULL AND previous_credential_version IS NOT NULL))
);
CREATE TABLE control_model_publications (
    model_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    provider_id TEXT NOT NULL REFERENCES control_model_providers(provider_id),
    upstream_model TEXT NOT NULL,
    enabled BIGINT NOT NULL CHECK (enabled IN (0, 1)),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL
);
CREATE INDEX control_model_publications_provider ON control_model_publications(provider_id);
CREATE TABLE control_model_groups (
    group_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    deleted_at_ms BIGINT
);
CREATE TABLE control_model_group_members (
    group_id TEXT NOT NULL REFERENCES control_model_groups(group_id),
    user_id TEXT NOT NULL REFERENCES control_users(user_id) ON DELETE CASCADE,
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (group_id, user_id)
);
CREATE INDEX control_model_group_members_user ON control_model_group_members(user_id, group_id);
CREATE TABLE control_model_grants (
    grant_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('user', 'group')),
    subject_id TEXT NOT NULL,
    monthly_tokens BIGINT NOT NULL CHECK (monthly_tokens > 0),
    max_concurrent_requests BIGINT NOT NULL CHECK (max_concurrent_requests > 0),
    expires_at_ms BIGINT,
    allow_resource_sharing BIGINT NOT NULL DEFAULT 1 CHECK (allow_resource_sharing IN (0,1)),
    revoked_at_ms BIGINT,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL
);
CREATE INDEX control_model_grants_subject ON control_model_grants(subject_kind, subject_id, grant_id);
CREATE TABLE control_model_grant_models (
    grant_id TEXT NOT NULL REFERENCES control_model_grants(grant_id),
    model_id TEXT NOT NULL REFERENCES control_model_publications(model_id),
    PRIMARY KEY (grant_id, model_id)
);
CREATE TABLE control_model_keys (
    key_id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    token_prefix TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    grant_id TEXT NOT NULL REFERENCES control_model_grants(grant_id),
    name TEXT NOT NULL,
    model_ids_json TEXT NOT NULL,
    monthly_tokens BIGINT CHECK (monthly_tokens > 0),
    max_concurrent_requests BIGINT CHECK (max_concurrent_requests > 0),
    expires_at_ms BIGINT,
    revoked_at_ms BIGINT,
    created_at_ms BIGINT NOT NULL,
    last_used_at_ms BIGINT
);
CREATE INDEX control_model_keys_user ON control_model_keys(user_id, key_id);
CREATE TABLE control_model_requests (
    request_id TEXT PRIMARY KEY,
    origin TEXT NOT NULL CHECK (origin IN ('api_key','client_device','workload')),
    source TEXT NOT NULL CHECK (source IN ('platform_grant','user_provider')),
    caller_scope TEXT NOT NULL,
    key_id TEXT,
    request_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    actor_user_id TEXT NOT NULL,
    resource_owner_user_id TEXT,
    model_beneficiary_user_id TEXT NOT NULL,
    grant_id TEXT,
    grant_name TEXT,
    workload_json TEXT,
    tenant_id TEXT,
    project_id TEXT,
    session_id TEXT,
    run_id TEXT,
    execution_reservation_id TEXT,
    budget_period_start TEXT,
    model_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    upstream_model TEXT NOT NULL,
    protocol TEXT NOT NULL,
    route_snapshot_json TEXT NOT NULL,
    max_attempts BIGINT NOT NULL CHECK (max_attempts BETWEEN 1 AND 8),
    state TEXT NOT NULL CHECK (state IN ('pending','completed','failed','cancelled')),
    error_code TEXT,
    month TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    settled_at_ms BIGINT,
    UNIQUE (caller_scope, request_key),
    CHECK ((origin IN ('api_key','client_device') AND key_id IS NOT NULL AND workload_json IS NULL)
        OR (origin='workload' AND key_id IS NULL AND workload_json IS NOT NULL)),
    CHECK ((source='platform_grant' AND grant_id IS NOT NULL)
        OR (source='user_provider' AND grant_id IS NULL))
);
CREATE TABLE control_model_attempts (
    request_id TEXT NOT NULL REFERENCES control_model_requests(request_id),
    attempt BIGINT NOT NULL CHECK (attempt BETWEEN 1 AND 8),
    state TEXT NOT NULL CHECK (state IN ('pending','completed','failed','cancelled')),
    attempted BIGINT NOT NULL DEFAULT 0 CHECK (attempted IN (0,1)),
    reserved_tokens BIGINT NOT NULL CHECK (reserved_tokens > 0),
    accounted_tokens BIGINT CHECK (accounted_tokens >= 0),
    usage_json TEXT,
    input_tokens BIGINT,
    output_tokens BIGINT,
    cached_input_tokens BIGINT,
    cache_write_tokens BIGINT,
    reasoning_tokens BIGINT,
    upstream_request_id TEXT,
    error_code TEXT,
    created_at_ms BIGINT NOT NULL,
    settled_at_ms BIGINT,
    PRIMARY KEY (request_id, attempt)
);
CREATE INDEX control_model_requests_grant_month ON control_model_requests(grant_id, month);
CREATE INDEX control_model_requests_key_month ON control_model_requests(key_id, month);
CREATE INDEX control_model_requests_actor ON control_model_requests(actor_user_id, created_at_ms DESC, request_id DESC);
CREATE INDEX control_model_requests_beneficiary ON control_model_requests(model_beneficiary_user_id, created_at_ms DESC, request_id DESC);
CREATE INDEX control_model_requests_time ON control_model_requests(created_at_ms DESC, request_id DESC);
CREATE INDEX control_model_requests_pending ON control_model_requests(state, expires_at_ms, request_id);
CREATE INDEX control_model_requests_reservation ON control_model_requests(tenant_id, execution_reservation_id);
CREATE INDEX control_model_requests_budget ON control_model_requests(tenant_id, budget_period_start);
CREATE INDEX control_model_requests_run ON control_model_requests(tenant_id, run_id);

CREATE TABLE control_model_device_authorizations (
    device_hash TEXT PRIMARY KEY,
    user_code_hash TEXT NOT NULL UNIQUE,
    device_name TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','approved','denied','consumed')),
    user_id TEXT REFERENCES control_users(user_id),
    scope_json TEXT,
    limits_json TEXT NOT NULL DEFAULT '{}',
    expires_at_ms BIGINT NOT NULL,
    next_poll_at_ms BIGINT NOT NULL,
    interval_ms BIGINT NOT NULL DEFAULT 5000,
    created_at_ms BIGINT NOT NULL
);
CREATE INDEX control_model_device_authorizations_expiry ON control_model_device_authorizations(expires_at_ms);

CREATE TABLE control_model_devices (
    device_id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id),
    device_name TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    limits_json TEXT NOT NULL DEFAULT '{}',
    revoked_at_ms BIGINT,
    created_at_ms BIGINT NOT NULL,
    last_used_at_ms BIGINT
);
CREATE INDEX control_model_devices_user ON control_model_devices(user_id, device_id);
