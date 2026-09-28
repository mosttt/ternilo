CREATE FUNCTION ternilo_reject_audit_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'control_audit_log is append-only';
END;
$$;

CREATE TRIGGER control_audit_log_immutable
BEFORE UPDATE OR DELETE ON control_audit_log
FOR EACH ROW EXECUTE FUNCTION ternilo_reject_audit_mutation();

CREATE FUNCTION ternilo_enrollment_tenant(p_token_hash BYTEA)
RETURNS TEXT LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp
AS $$ SELECT tenant_id FROM control_executor_enrollments WHERE token_hash = p_token_hash $$;
CREATE FUNCTION ternilo_node_credential_tenant(p_token_hash BYTEA)
RETURNS TEXT LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp
AS $$ SELECT tenant_id FROM control_node_credentials WHERE token_hash = p_token_hash $$;
CREATE FUNCTION ternilo_list_user_tenants(p_user_id TEXT)
RETURNS TABLE (tenant_id TEXT, slug TEXT, display_name TEXT, role TEXT, kind TEXT)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
    SELECT tenant.tenant_id, tenant.slug, tenant.display_name, membership.role, tenant.kind
    FROM control_memberships AS membership
    JOIN control_tenants AS tenant USING (tenant_id)
    WHERE membership.user_id = p_user_id
    ORDER BY tenant.created_at_ms, tenant.tenant_id
$$;

CREATE FUNCTION ternilo_account_credential_tenants(p_user_id TEXT)
RETURNS TABLE (tenant_id TEXT)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp
AS $$
    SELECT tenant_id FROM control_executors WHERE owner_user_id = p_user_id
    UNION SELECT tenant_id FROM control_executor_enrollments WHERE created_by = p_user_id
    ORDER BY tenant_id
$$;
REVOKE ALL ON FUNCTION ternilo_account_credential_tenants(TEXT) FROM PUBLIC;

REVOKE ALL ON FUNCTION ternilo_enrollment_tenant(BYTEA) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_node_credential_tenant(BYTEA) FROM PUBLIC;
REVOKE ALL ON FUNCTION ternilo_list_user_tenants(TEXT) FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT EXECUTE ON FUNCTION ternilo_account_credential_tenants(TEXT), ternilo_enrollment_tenant(BYTEA), ternilo_node_credential_tenant(BYTEA), ternilo_list_user_tenants(TEXT) TO ternilo_runtime;
END IF;
END $$;
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
ALTER TABLE control_edge_input_provenance ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_edge_session_provenance ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_edge_upload_streams ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_edge_deleted_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_edge_session_uploads ENABLE ROW LEVEL SECURITY;
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

CREATE POLICY control_edge_deleted_sessions_scope ON control_edge_deleted_sessions
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_edge_session_provenance_scope ON control_edge_session_provenance
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_edge_input_provenance_scope ON control_edge_input_provenance
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_edge_upload_streams_scope ON control_edge_upload_streams
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_edge_session_uploads_scope ON control_edge_session_uploads
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

ALTER TABLE control_extension_publishers ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_extension_packages ENABLE ROW LEVEL SECURITY;

CREATE POLICY control_extension_publishers_scope ON control_extension_publishers
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE POLICY control_extension_packages_scope ON control_extension_packages
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

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

ALTER TABLE control_user_provider_profiles ENABLE ROW LEVEL SECURITY;

CREATE POLICY control_user_provider_profiles_scope ON control_user_provider_profiles
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT SELECT ON ternilo_schema TO ternilo_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON control_users, control_tenants, control_memberships, control_projects, control_quotas, control_quota_usage, control_quota_reservations, control_executor_enrollments, control_executors, control_node_credentials, control_workspaces, control_edge_executors, control_edge_events, control_edge_input_provenance, control_edge_session_provenance, control_edge_sessions, control_edge_upload_streams, control_edge_deleted_sessions, control_edge_session_uploads, control_secrets, control_secret_heads, control_audit_log, control_extension_publishers, control_extension_packages, control_user_agent_presets, control_user_preferences, control_user_credentials, control_user_credential_records, control_user_provider_profiles, control_instance_settings, control_native_accounts, control_browser_sessions, control_user_invitations, control_resource_shares, control_permission_groups, control_permission_group_members, control_resource_group_shares, control_resource_fork_group_sources TO ternilo_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON control_account_spaces TO ternilo_runtime;
GRANT SELECT, INSERT ON control_platform_audit TO ternilo_runtime;
GRANT USAGE, SELECT ON SEQUENCE control_audit_log_audit_sequence_seq TO ternilo_runtime;
END IF;
END $$;

ALTER TABLE control_resource_shares ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_resource_shares_scope ON control_resource_shares
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

CREATE TRIGGER control_platform_audit_immutable
BEFORE UPDATE OR DELETE ON control_platform_audit
FOR EACH ROW EXECUTE FUNCTION ternilo_reject_audit_mutation();

ALTER TABLE control_permission_groups ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_permission_groups_scope ON control_permission_groups
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

ALTER TABLE control_permission_group_members ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_permission_group_members_scope ON control_permission_group_members
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

ALTER TABLE control_resource_group_shares ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_resource_group_shares_scope ON control_resource_group_shares
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

ALTER TABLE control_resource_fork_group_sources ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_resource_fork_group_sources_scope ON control_resource_fork_group_sources
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));

ALTER TABLE control_model_providers ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_providers_service ON control_model_providers
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

ALTER TABLE control_model_publications ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_publications_service ON control_model_publications
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

ALTER TABLE control_model_groups ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_groups_service ON control_model_groups
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

ALTER TABLE control_model_group_members ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_group_members_service ON control_model_group_members
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

ALTER TABLE control_model_grants ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_grants_service ON control_model_grants
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

ALTER TABLE control_model_grant_models ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_grant_models_service ON control_model_grant_models
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

ALTER TABLE control_model_keys ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_keys_service ON control_model_keys
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

ALTER TABLE control_model_requests ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_requests_service ON control_model_requests
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');

DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON control_model_providers, control_model_publications, control_model_groups, control_model_group_members, control_model_grants, control_model_grant_models, control_model_keys, control_model_requests TO ternilo_runtime;
END IF;
END $$;

ALTER TABLE control_model_attempts ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_attempts_service ON control_model_attempts
USING (current_setting('ternilo.model_service', true) = 'on')
WITH CHECK (current_setting('ternilo.model_service', true) = 'on');
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON control_model_attempts TO ternilo_runtime;
END IF;
END $$;

ALTER TABLE control_model_device_authorizations ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_device_authorizations_service ON control_model_device_authorizations
    USING (current_setting('ternilo.model_service', true) = 'on')
    WITH CHECK (current_setting('ternilo.model_service', true) = 'on');
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON control_model_device_authorizations TO ternilo_runtime;
END IF;
END $$;

ALTER TABLE control_model_devices ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_model_devices_service ON control_model_devices
    USING (current_setting('ternilo.model_service', true) = 'on')
    WITH CHECK (current_setting('ternilo.model_service', true) = 'on');
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON control_model_devices TO ternilo_runtime;
END IF;
END $$;
