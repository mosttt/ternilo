ALTER TABLE control_service_accounts ENABLE ROW LEVEL SECURITY;
ALTER TABLE control_service_credentials ENABLE ROW LEVEL SECURITY;
CREATE POLICY control_service_accounts_scope ON control_service_accounts
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
CREATE POLICY control_service_credentials_scope ON control_service_credentials
USING (tenant_id = current_setting('ternilo.tenant_id', true))
WITH CHECK (tenant_id = current_setting('ternilo.tenant_id', true));
REVOKE ALL ON control_service_accounts,control_service_credentials FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_service_accounts,control_service_credentials TO ternilo_runtime;
END IF;
END $$;
