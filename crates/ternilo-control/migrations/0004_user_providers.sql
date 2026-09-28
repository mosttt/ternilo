CREATE TABLE control_user_provider_profiles (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_json JSONB NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY (tenant_id, user_id, provider_id),
    FOREIGN KEY (tenant_id, user_id)
        REFERENCES control_memberships(tenant_id, user_id) ON DELETE CASCADE
);

ALTER TABLE control_user_provider_profiles ENABLE ROW LEVEL SECURITY;

CREATE POLICY control_user_provider_profiles_scope ON control_user_provider_profiles
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE OR REPLACE FUNCTION ternilo_user_model_route_for_worker(
    p_tenant_id TEXT,
    p_user_id TEXT,
    p_provider_id TEXT
)
RETURNS TABLE (
    provider_json JSONB,
    credential_name TEXT,
    credential_version BIGINT,
    credential_nonce BYTEA,
    credential_ciphertext BYTEA
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT
        provider.provider_json,
        provider.provider_json ->> 'api_key_ref',
        credential.version,
        credential.nonce,
        credential.ciphertext
    FROM control_user_provider_profiles AS provider
    LEFT JOIN control_user_credentials AS credential
      ON credential.tenant_id = provider.tenant_id
     AND credential.user_id = provider.user_id
     AND credential.name = provider.provider_json ->> 'api_key_ref'
    WHERE provider.tenant_id = p_tenant_id
      AND provider.user_id = p_user_id
      AND provider.provider_id = p_provider_id;
$$;

REVOKE ALL ON FUNCTION ternilo_user_model_route_for_worker(TEXT, TEXT, TEXT) FROM PUBLIC;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE ON control_user_provider_profiles
            TO ternilo_runtime;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_worker') THEN
        GRANT EXECUTE ON FUNCTION ternilo_user_model_route_for_worker(TEXT, TEXT, TEXT)
            TO ternilo_worker;
    END IF;
END;
$$;
