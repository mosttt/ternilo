CREATE TABLE control_user_agent_presets (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    preset_id TEXT NOT NULL,
    document_json JSONB NOT NULL,
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
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 24),
    ciphertext BYTEA NOT NULL,
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
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 24),
    ciphertext BYTEA NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, record_key),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);

ALTER TABLE control_user_agent_presets ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_user_preferences ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_user_credentials ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_user_credential_records ENABLE ROW LEVEL SECURITY;

CREATE POLICY control_user_agent_presets_scope ON control_user_agent_presets
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_user_preferences_scope ON control_user_preferences
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_user_credentials_scope ON control_user_credentials
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_user_credential_records_scope ON control_user_credential_records
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE ON
            control_user_agent_presets,
            control_user_preferences,
            control_user_credentials,
            control_user_credential_records
        TO ternilo_runtime;
    END IF;
END;
$$;
